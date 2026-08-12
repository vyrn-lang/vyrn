# Audit: the Vyrn website, five lenses

An external review of the Vyrn website on `website-v1` at `e832e66`. Five
reviewers, each with its own values, each hunting what only that reviewer would
find.

Every finding carries evidence. A code finding cites `file:line`. A behavioural
finding carries a reproduction that was run: the site was exported with the
release binary, served on a fixed port, and driven in a browser. Findings are
ranked **CONFIRMED** (reproduced or measured) above **PLAUSIBLE** (argued from
reading, not run).

The design brief (`docs/research/website-brief.md`) was read first. Where the
brief records a decision with its argument, the entry says "design critique",
not "defect".

**How the site was exercised.** `site std examples rfcs web` were copied to a
scratch tree, `out/docs/std` pre-created, and the site exported with
`vyrn run site/export.vyrn out` (41 routes, 7 assets, exit 0). The site's own
tests pass (10 + 9 + 15 + 3 + 3 + 8 + 7 + 6 = 61 across eight modules). The
export was served over `http.server` and crawled. The hero wasm module
(`examples/herofield.vyrn`) could not be built **in this sandbox only**, because
the compiler resolves `std/` relative to its own binary — to the shared checkout
on `main`, whose `std/math.vyrn` predates the `sin`/`cos`/`floorF` this branch
adds. On a `website-v1` checkout the build is clean, so the pages were driven
with the hero's JavaScript fallback (its intended circuit-breaker path).

---

## Top 12 by severity

| # | Severity | Lens | Finding | Ref |
|---|---|---|---|---|
| 1 | **High** | PL | **The signature parity widget executes nothing.** The pill says "Live check" and the button "Run all three", but `run()` is a 900 ms timer that reveals a hardcoded digest; "break one byte" flips a character in a constant string. It is the picture of a green tick the brief's own W2 said not to build. | W5.1 |
| 2 | Medium | C systems | **`hero.js` leaks listeners on every soft navigation.** `mountHero` registers a window `resize`, a `visibilitychange`, and a `matchMedia` `change` listener, and `boot()` re-runs it on every `vyrn:nav-end`. Measured: two boots add two of each, none removed, each holding a detached canvas. | W2.1 |
| 3 | Medium | Agda / PL | **Reference pages have 143 copy-button tab stops and no skip link.** Every inline `<code>` becomes `role="button" tabindex="0"`; `std/http` has 143 of 222 tab stops, reached only after the masthead and a 23-link rail. | W5.3 |
| 4 | Medium | PL | **The tab widgets are an incomplete ARIA pattern.** 12 `role="tab"`, but no `role="tablist"`, no `role="tabpanel"`, no `aria-controls`, no ids, and `ArrowRight` moves nothing. | W5.2 |
| 5 | Medium | PL | **No page declares a language.** `<html>` carries no `lang` attribute on any of the 41 pages — WCAG 3.1.1, on every page. | W5.4 |
| 6 | Medium | Agda | **The "every href resolves" property is total but half-gated.** An independent crawl of 41 pages found 0 broken links and 0 broken anchors, but the export tests only the generated `#e-` anchors; every hand-authored cross-page fragment resolves by author discipline, not by a gate. | W4.1 |
| 7 | Low | Rust | **`escapeHtml` is not attribute-safe by contract.** It omits `"` and `'` deliberately; it is sound only because every one of its call sites lands in element content. A future signature routed into an attribute breaks out. | W3.1 |
| 8 | Low | PL | **The footer prints a hand-written `0 divergences`.** A stat the site's own data rule forbids: if a divergence ever exists, the footer lies. | W5.6 |
| 9 | Low | Rust | **The export swallows its I/O error reasons.** `emit()` and `copyAsset()` map `Err(why)` to a bare count, and `copyAsset` reports "unreadable or empty" for two different failures. | W3.2 |
| 10 | Low | Linus | **The generated reference is nine linear `if`-chains.** Each `docSummary`/`docNames`/… is an O(modules) scan, and a module page calls several per module — O(n²) where a generated map would be O(1). | W1.1 |
| 11 | Low | Rust | **`rfcHref` cannot round-trip an RFC ≥ 1000.** `rfcTag` pads to five digits where the file name has four. All 98 present RFCs round-trip; this is the shape, not a live break. | W3.3 |
| 12 | Low | PL | **No favicon.** None is shipped or linked, so every page 404s `/favicon.ico`, while `editor/vscode/icons/vyrn.svg` sits unused. | W5.5 |

Two results sit above the whole table and are the most important sentences in
it. **Every internal link and every anchor resolves** — 0 broken across 41
pages, crawled independently of the export's own tests (W4.1). And **the XSS
surface is closed**: there is no runtime path from untrusted input to
`innerHTML`, and a malicious fragment was injected and produced no node (W4.2).
This is a site that tells the truth in its markup; the findings above are about
where the *chrome* around that truth overclaims or excludes a reader.

---

## Lens 1 — Linus Torvalds: systems pragmatism and taste

The export is fast and its output is compact: 41 routes and 7 assets written in
a few seconds, the largest page 39 KB, no quadratic string building anywhere in
the emitters. Most of what this lens looks for is simply absent, which is worth
saying before the two low findings.

### W1.1 CONFIRMED — Low. The generated reference is nine linear `if`-chains

`site/app/apidoc.vyrn:416-427`. `lookup` emits
`export fn NAME(m: String) -> RET { <branches> return FALLBACK }` and `branch`
(`apidoc.vyrn:425`) emits `    if m == "..." { return ... }`. Nine functions are
built this way — `docSummary`, `docIntro`, `docNames`, `docKinds`, `docSigs`,
`docProse`, `docLines`, `docCount`, plus `docModules`. Every lookup is an
O(modules) scan.

`apiModules` (`apidoc.vyrn:42`) calls `docSummary(m)` and `docCount(m)` per
module, and `docs/index.vyx`'s `exportCount()` calls `m.count` after building the
whole list — so a page render is O(n²) in the 35 modules. At n = 35 this is
nothing, and that is why it is Low. It is still a data structure fighting the
problem: the generator has every fact in hand at emit time and could emit one
array indexed by position, or a `Map`, instead of nine scans.

### W1.2 PLAUSIBLE — Low. `export.vyrn` recomputes the route table

`site/export.vyrn:166` iterates `sitePaths()` and `site/export.vyrn:176` calls
`sitePaths().length` again for the final line — two full recomputations, each
re-running `apiModules()` (finding W1.1). `markCurrent` (`export.vyrn:94`) copies
the whole document with two `substring`s per page to stamp one attribute. Both
are trivial at this size and neither is worth a change; recorded because a larger
`std/` makes W1.1 the thing to watch.

### What this lens found clean

- **No quadratic string building.** Every generator (`hl.vyrn`, `apidoc.vyrn`,
  `chart.vyrn`) pushes into an `Array<String>` and joins once, or appends to one
  buffer left to right. No `insert(0, ..)`, no re-scan of a growing string.
- **The highlighter is one left-to-right pass** over the compiler's own tokens
  (`hl.vyrn:340`), lexing each snippet exactly once at generation time.
- **The export writes each file once** and counts failures rather than
  retrying; a failed route or a missing asset makes the whole run exit non-zero
  (`export.vyrn:172`), so a broken build cannot publish a hole.

---

## Lens 2 — C systems: resource discipline

The Vyrn side is clean on this lens — the export is a straight-line program over
`readFile`/`writeFile` with no regions, no `consume` handovers, and no manual
frees to get wrong. The finding is in the hand-written JavaScript.

### W2.1 CONFIRMED — Medium. `hero.js` leaks listeners on every soft navigation

`site/public/hero.js:173-179`. `mountHero` registers, on the window and the
document rather than on the canvas it can drop:

```js
new ResizeObserver(remeasure).observe(canvas);
addEventListener("resize", remeasure, { passive: true });
new IntersectionObserver(...).observe(canvas);
document.addEventListener("visibilitychange", () => ...);
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", remeasure);
```

`site/public/widgets.js:620` calls `mountHero` inside `boot()`, and
`widgets.js:628` runs `boot()` again on every `vyrn:nav-end`. The landing page
is the one page with `#field`, so every soft navigation back to it runs
`mountHero` again.

**Measured.** Instrumenting `window.addEventListener` and the `matchMedia`
change registration, then dispatching `vyrn:nav-end` twice:

```
windowListenerAddsAfter2Boots: { resize: 2 }
matchMediaChangeAdds: 2
```

The two `ResizeObserver`/`IntersectionObserver` instances are collected with the
replaced canvas, but the window `resize`, the document `visibilitychange`, and
the `matchMedia` `change` listeners are not — each closes over the *old* detached
canvas, its `ctx`, and its `INK` table, and each fires `measure()`+`paint()` on
that detached canvas on every real resize or theme toggle. Over a browsing
session the count grows without bound.

The irony is that `widgets.js` gets this exactly right and says so: its
`pointerdown`/`click`/`keydown`/`scroll`/`resize` listeners are registered at
module scope precisely so `boot()` cannot stack them (`widgets.js:225`,
`:564`, comments at `:562`, `:626`). `hero.js` is the one module that registers
document- and window-level listeners inside the per-mount function.

**Fix.** Return the teardown from `mountHero` (it already returns `{start,
stop}`), remove the window/document/matchMedia listeners in it, and have `boot()`
call it before re-mounting; or register them once at module scope like
`widgets.js` does.

### What this lens found clean

- **`onView`'s `IntersectionObserver` disconnects after firing once**
  (`widgets.js:38`), so the per-widget observers do not accumulate.
- **`fresh.js` holds nothing.** One fetch, an hour of `localStorage` caching, no
  timer, no retry, and every failure path returns quietly (`fresh.js:16-39`).
- **The hero's fill-string table is built once per theme** (`hero.js:68`), not
  per cell, which is the allocation the brief called out.

---

## Lens 3 — Rust reviewer: correctness and API taste

The Vyrn is disciplined: `match` over every `Result`, `.copy()` where a value
crosses into an owned field, and no reachable trap on site data — the numbers
are file counts and byte sizes, never user input. The findings are about
contracts that hold by circumstance rather than by type.

### W3.1 CONFIRMED — Low. `escapeHtml` is not attribute-safe by contract

`site/app/code.vyrn:37-67`. `escapeByte` replaces `&`, `<` and `>` and nothing
else — the comment defends it: "a quote inside a `<pre>` is text". That is true
for the element-content contexts where it is used, and the audit checked every
one: signatures, prose, summaries and diagnostics all reach the page through
`v-html` into element content, never into an attribute.

The one attribute that carries highlighted markup with quotes — `:data-alt` on
the types specimen (`routes/index.vyx:91`) — is escaped by `std/vyx`, not by this
function, and the quotes survive intact (verified in the DOM:
`data-alt` reads `<span class="k">let</span> bad = ...`). So the site is sound
today. But `escapeHtml` is the site's general HTML-escaper by name, and it is
not safe for the job its name implies: the day a signature or a title is routed
into an attribute, an unescaped `"` breaks out. The safety is a property of the
call sites, not of the function.

**Fix.** Escape `"` (and `'`) too, or rename it `escapeText` and document that
attribute values go through `std/vyx`.

### W3.2 CONFIRMED — Low. The export swallows its I/O error reasons

`site/export.vyrn:107-112` and `:150-160`. `emit` binds `Err(why)` and returns
`1`, discarding `why`. `copyAsset` prints `FAIL <source>: unreadable or empty`
for a read error *and* for a genuinely empty file — two different failures under
one message. When the CI export fails, the operator gets a count and a
conflated label, not the OS reason.

**Fix.** Print `why` in the `FAIL` line, and separate the empty-file case from
the unreadable one.

### W3.3 PLAUSIBLE — Low. `rfcHref` cannot round-trip an RFC ≥ 1000

`site/app/chart.vyrn:380-397`. `rfcTag` returns `"RFC-0" + n.toString()` for
`n >= 100`, which is five digits for `n >= 1000`, where the file name has four
(`RFC-1000-...`). `rfcOf` (`facts.vyrn:79`) reads the number from a fixed
four-character window (`substring(name, 4, 8)`), so the two disagree above 999.
All 98 present RFC links reconstruct to real files (verified against the
repository), so this is the shape of the code, not a live break.

### W3.4 Design critique. Chart geometry is String-typed throughout

`site/app/chart.vyrn:40`. `Bar`, `Grid`, `Node` and `Cell` hold every
coordinate as a `String`, including numbers. The module defends it at
`chart.vyrn:29`: "Every field is a `String`, because that is what an SVG
attribute is. Rounding happens here, once, rather than in four `.toString()`
calls at the template." That is a reasonable trade for a build-time geometry
module, and it stands. Recorded because it is the kind of string-typing the Rust
lens usually flags, and here it is argued.

### What this lens found clean

- **No reachable trap on site data.** Every index into a generated array is
  bounded by that array's own `.length` in the same function; the numbers are
  file counts and measured byte sizes, not user input.
- **`match` over every effect.** `listDir` and `readFile` are always matched,
  never unwrapped (`facts.vyrn:47`, `apidoc.vyrn:431`, `export.vyrn:108`), and a
  failure degrades to an empty listing or a zero count that is visible on the
  page rather than a panic.

---

## Lens 4 — Agda implementer: soundness

The brief for this lens was to break the export's central claim — that every
link it writes resolves, and that nothing untrusted reaches the DOM. Both held
under attack, and the negative results are the finding.

### W4.1 CONFIRMED — Medium. "Every href resolves" is total, but only half-gated

An independent crawl parsed all 41 exported HTML files, resolved every `href`
and `src` against the output tree, and checked every `#fragment` — in-page and
cross-page — against the ids on its target page.

```
pages crawled: 41
BROKEN INTERNAL HREF (0)
BROKEN SRC (0)
BROKEN ANCHORS (0)
```

The property is total: no dead internal link, no dangling anchor, including the
cross-page fragments (`/index.html#memory`, `/compare.html#numbers`,
`/docs.html#std`, `/install.html#check`). And 489 external `blob/main` links —
98 of them RFC links — every one reconstructs to a real file or directory in the
repository.

The gap is what *guarantees* it. `export.vyrn`'s own tests cover the router's
generated links and the `#e-<name>` reference anchors
(`export.vyrn:188-263`). The cross-page fragments above are hand-written in the
`.vyx` templates (`routes/index.vyx:169`, `docs/index.vyx:54-57`), and nothing
tests that `#numbers` still exists on `/compare` after a refactor. Today they all
resolve; they resolve by author discipline, not by the gate the rest of the site
is proud of.

**Fix.** Extend the export's link test to collect every in-page `href="#..."`
and every `href="/x.html#..."` across all rendered documents and assert the
target id exists — the crawl this audit ran, as a `test` block.

### W4.2 CONFIRMED — Medium (as a negative result). The XSS surface is closed

There is exactly one `innerHTML` write in the site's own scripts — `typesWidget`
(`widgets.js:385`) — and it assigns from `row.dataset.alt`, which the export
wrote at build time from the compiler's own lexer. `fresh.js` is the only code
that touches the network, and it writes the release tag with `textContent` and
the URL with `setAttribute("href", ...)` (`fresh.js:55-57`), where the URL is
GitHub's own `html_url` (always `https://github.com/...`). The fragment path in
`widgets.js` is `getElementById(decodeURIComponent(hash))` (`widgets.js:556`) —
a lookup, never a sink.

**Attempted break.** A malicious fragment was set and the scroll handler fired:

```js
location.hash = '#"><img src=x onerror=alert(1)>';
window.dispatchEvent(new Event('scroll'));
// document.querySelector('img[src="x"]')  -> null
// body contains no onerror  -> true
```

No node was created. The brief names the `#c=` playground contract and the
`#e-N` anchors as attack surface; the playground is v1.1 and does not exist in
this export, and the `#e-N` anchors are built from `std/` export identifiers,
which cannot carry a quote or an angle bracket. The one attribute that carries
build-time markup (`:data-alt`) is escaped by `std/vyx` (W3.1). There is no
reachable XSS in v1.

### What this lens could not break

- A malicious `location.hash` reaching `innerHTML` — refused; the only consumer
  is `getElementById`.
- The release feed reaching `innerHTML` — refused; `fresh.js` uses `textContent`
  and a GitHub-origin `href`.
- A snippet carrying markup out of its `<pre>` — refused; `escapeHtml` closes
  `<`, `>`, `&`, and the code lens's own test asserts it (`code.vyrn:240`).

---

## Lens 5 — PL / product theory: coherence

The site's thesis is that Vyrn tells the truth and shows it running. The prose
holds to it. The interactive chrome does not always, and the accessibility gaps
exclude a class of reader the prose is trying to convince.

### W5.1 CONFIRMED — High. The parity widget executes nothing

`routes/index.vyx:176-205` and `widgets.js:274-318`. The plate wears a
`class="pill live">Live check` pill and a `Run all three` button. The reader's
model is that pressing it runs the three backends. It does not. `run()` is:

```js
cols.classList.add("arming");
setTimeout(() => cols.classList.remove("arming"), 120);
setTimeout(settle, 900);   // the pause is the whole widget
```

`settle` writes a **hardcoded** digest (`4605754516372531133`, three times,
straight from the markup) and the string `"0 bytes differ"`. "Break one byte"
does `good.slice(0, -1) + (good.endsWith("3") ? "7" : "3")` — it flips the last
character of a constant and compares two strings.

**Observed.** Pressing "Break one byte" then waiting: the verdict went
`0 bytes differ` → `1 byte differs — the harness fails` and the wasm column
`...531133` → `...531137`. A string edit and a timer.

The brief's W2 is explicit that this must not happen: the wasm column is to be
"executed by `wasi-min.js` on the spot … so the reader sees that the check is
real and not a picture of a green tick." The shipped widget is that picture. The
underlying claim is true — parity is real and CI-enforced, the caption gives the
`cargo test` command, and the digest is presumably genuine — which is why this
is a coherence defect and not a lie. But on the one page whose argument is
"believe the measurement", the flagship widget animates a constant behind a
"Live" label.

**Fix.** Either run the wasm column through `wasi-min.js` as the brief specifies
(the module and the shim both ship), or drop the "Live" pill and the "Run" verb
and present it as the static, CI-backed digest it is.

### W5.2 CONFIRMED — Medium. The tab widgets are an incomplete ARIA pattern

Measured on `/compare` (the pattern is shared by `/philosophy` and `/install`):

```
role="tab": 12   role="tablist": 0   role="tabpanel": 0
aria-controls on tabs: 0   ids on tabs: 0   ArrowRight moves selection: false
```

`routes/compare.vyx:146` renders `<button role="tab" aria-selected>` with no
enclosing `role="tablist"`, and the panes (`compare.vyx:149`) are plain `<div>`
with no `role="tabpanel"` and no `aria-controls`/`id` pairing. `tabsWidget`
(`widgets.js:487`) wires click only. A screen reader announces a "tab" with no
group and no controlled panel; a keyboard user gets no arrow-key movement, which
the WAI-ARIA tabs pattern requires. The buttons are still operable (they are
buttons), so this is a degraded experience, not a dead one.

**Fix.** `role="tablist"` on `.tabs`, `role="tabpanel"` + `aria-labelledby` on
each pane, `id` + `aria-controls` pairing, and roving `tabindex` with
arrow-key handling in `tabsWidget`.

### W5.3 CONFIRMED — Medium. Reference pages flood the tab order, with no skip link

Every inline `<code>` is turned into a copy control at boot: `markCopyable`
(`widgets.js:208-216`) sets `tabIndex = 0` and `role="button"` on each. Measured
on `/docs/std/http`:

```
code buttons in tab order: 143
total tab stops: 222
skip-to-content link: none
```

A keyboard or switch user reaching the third export on that page tabs through the
masthead, a 23-link rail, and 143 "Copy …" buttons interleaved with the prose,
with no skip link to jump the furniture. The copy-on-click affordance is a good
mouse feature; it is charged in full to the keyboard reader, and the reference
pages — the longest on the site — pay the most.

**Fix.** A skip-to-content link as the first focusable element (the brief's own
`#main` shape), and consider making a code span copyable without entering the tab
order — activate on hover/focus, or move copy to a single delegated affordance
rather than 143 tab stops.

### W5.4 CONFIRMED — Medium. No page declares a language

The exported `<html>` is bare — `<html><head><meta charset="utf-8">…` — with no
`lang` attribute, on all 41 pages. WCAG 3.1.1 (Language of Page, Level A) fails
everywhere. The document skeleton comes from `std/ui`, and the site's `pageHead`
(`nav.vyrn:21`) adds a viewport and a description meta but no `lang`, so nothing
sets it. A screen reader guesses the voice.

**Fix.** Have the layout or `pageHead` emit `lang="en"` on the document element.

### W5.5 CONFIRMED — Low. No favicon

No `<link rel="icon">` is emitted and no icon asset is copied, so every page
requests and 404s `/favicon.ico` (confirmed by fetch). The project has a brand
mark — `editor/vscode/icons/vyrn.svg`, the same one the palette is sampled from
(`style.css:8`) — and it is not on the site's own tab.

**Fix.** Add the SVG to `assets()` (`export.vyrn:136`) and a `<link rel="icon">`
to the layout head.

### W5.6 CONFIRMED — Low. The footer prints a hand-written statistic

`routes/layout.vyx:33`: `{{ exampleCount() }} examples, {{ backendCount() }}
backends, 0 divergences`. The example and backend counts are generated; `0
divergences` is a string literal. The site's own data rule (brief §1, and
`facts.vyrn:3`) is that no widget renders a number the project cannot produce on
demand. The divergence count is exactly such a number — the parity harness
produces it — and here it is asserted, on every page, by hand. If a divergence
ever exists, the footer contradicts it silently.

**Fix.** Either drop the claim from the footer or generate it from the harness
like the other counts.

### What this lens found coherent

- **The hero's JavaScript fallback is a faithful transcription** of
  `examples/herofield.vyrn`: the noise product, the lattice, the rational seam,
  the crossing, the luminance mapping and the glyph-index arithmetic all match
  the Vyrn line for line (`hero.js:26-49` against `herofield.vyrn:57-124`). The
  only divergence is `Math.sin` versus Vyrn's polynomial `sin`, imperceptible in
  a decorative, `aria-hidden` glyph grid. The circuit breaker means the hero
  always paints.
- **The pages degrade honestly without JavaScript.** The parity strip's markup
  already reads `0 bytes differ`, every comparison pane is present and stacked,
  and every chart is pre-rendered SVG. The reader with no script still reads the
  page and sees the answer, which is the brief's v1-done bar.
- **The prose keeps the data rule where it counts.** `/compare`'s missing
  benchmark is stated as missing rather than faked (`compare.vyx:304`), the
  install page says plainly there is no binary yet, and `/releases` refuses to
  invent a changelog. The one lapse is the footer (W5.6).
- **The stylesheet is careful on the things this lens usually finds broken:**
  `:focus-visible` outlines, a `prefers-reduced-motion` block that flattens
  every transition and the parity strip's opacity, 44 px touch targets, tabular
  numerals on all data, and a colour ramp converted to `oklch` with the contrast
  reasoning written in (`style.css:44-51`, `:146`, `:793-798`, `:929`).

---

## What this audit did not cover

An honest list of where a defect could be and this audit did not look, or looked
and could not decide.

- **The hero wasm was not run.** The sandbox could not build it (a `std/`
  resolution artefact, not a site defect — see the header), so the hero was
  exercised only through its JavaScript fallback. Whether the compiled module's
  `heroFrame` agrees with the fallback bit for bit was not tested in a page;
  three-way parity for `herofield.vyrn` is a separate harness's job.
- **`fresh.js` was not driven against the live GitHub API.** Its no-network
  paths were read and its DOM writes checked, but the actual fetch, the
  `localStorage` cache expiry, and the rate-limit behaviour were not exercised.
- **Soft navigation was simulated, not driven end to end.** W2.1 was reproduced
  by dispatching `vyrn:nav-end`, which is what `vyrn-nav.js` fires; the full
  `vyrn-nav.js` fetch-and-swap over a static host was not stepped through.
- **Contrast was reasoned, not measured.** The palette is defined in `oklch`
  with a stated contrast argument; no ratio was computed against WCAG floors for
  the 12 px muted-mono labels, which are the most likely to fall short.
- **The `std/vyx` and `std/ui` internals were out of scope.** W3.1, W5.2 and
  W5.4 all touch behaviour those modules own (attribute escaping, the document
  skeleton, the tab markup); this audit judged the site's use of them, not their
  implementation.
- **One browser, one viewport.** The pages were driven in the preview browser at
  desktop width. The stylesheet's 1024/640 px reflow and its `(hover: none)` and
  `(pointer: coarse)` paths were read but not exercised on a real small viewport.
- **Severity is this audit's judgement.** No user was asked. Putting the parity
  widget's honesty above a listener leak is defensible; the ordering inside each
  band is not worth arguing about.
