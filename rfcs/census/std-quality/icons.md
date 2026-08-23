# std/icons.vyrn

Lines: 1233. Exports: 5. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

Exports are four `export gen fn` (`icons` :112, `iconsAt` :127, `iconsModule` :152, `iconProvider` :679) and one `export fn` (`iconAttrs` :818). `lit` (:805) is a `gen fn` but not exported. Everything except `iconAttrs` runs at generation time only.

## What this module is for

A caller pins an Iconify collection (one JSON file, via a `vyrn.json` dependency alias), names glyphs in a generated import — `import * as ic from icons("icons", "github rss")` — and gets one `fn <name>() -> Html` per glyph, an inline `<svg>` inheriting `currentColor` and font size. Misspelled names fail the build with the nearest real name. A project writes a three-line `<Icon>` provider over `iconProvider`, and `iconAttrs` rewrites a drawn glyph at the use site (`size`, `label`, `class`). The site draws every glyph this way today (`site/app/icons.vyrn:31`, `site/app/docshell.vyrn:32`).

## Findings

### 2. Algorithm complexity — LOW

What: glyph lookup linear-scans the whole collection once per named glyph, O(N·G) for N named glyphs in a G-glyph collection.
Where: `std/icons.vyrn:533-542`.
Evidence: proving loops are the two scans at `std/icons.vyrn:533-537` (icons) and `std/icons.vyrn:538-542` (aliases), run once per `resolveGlyph` call, which runs once per named glyph (`std/icons.vyrn:488`). Measured on a synthetic 200-entry collection, uncached whole-program wall time of `vyrn run`: 1 glyph named 561 ms, 50 glyphs 650 ms, 200 glyphs 749 ms — about 0.9–1.8 ms per extra glyph including compiling its generated function, no superlinear growth visible at these sizes. Command: `VYRN_NO_GEN_CACHE=1 compiler/target/release/vyrn run <scratch file>`. Direct native benching of the generator is not possible today: calling `iconsModule` in a bench body passes `vyrn bench --check` but fails native codegen link with `use of undefined value '@vyrn_iconsModule'` (observed with `compiler/target/release/vyrn bench <scratch>`).
Cost if unfixed: every generated import re-scans; the site pays it per tag through `site/app/icons.vyrn:31` on each clean build.
Smallest fix: index `icons` and `aliases` into a hash map once in `collectionOf` (:214). RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: the emit loop allocates a fresh reserved-name array for every named glyph it checks.
Where: `std/icons.vyrn:404`.
Evidence: `reservedNames()` builds a new six-element array per call (:447-449); line :404 calls it inside the `for want in wanted` loop (:392-429), so N named glyphs cause N allocations, although `taken` was already seeded from the same list once at :388-391. Allocation count proven by loop bounds; per-allocation cost not benched.
Cost if unfixed: one short-lived array per glyph per build; paid by every caller of `icons`/`iconsAt`/`iconsModule`, e.g. `site/app/docshell.vyrn:32`.
Smallest fix: bind `let reserved = reservedNames()` before the loop and use it at :404. RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — LOW

What: a misspelled glyph pays full-collection edit distance against every icon and alias name, and each miss does it again.
Where: `std/icons.vyrn:644-657`.
Evidence: `nearest` loops all `coll.icons` then all `coll.aliases`, computing `editDistance` per name (:645, :652) — worst case O(G·L²) per misspelled glyph, proven by those loop bounds. Measured: an import naming 15 entries where 5 misspell took 663 ms uncached whole-program versus 561 ms baseline for the same program with 15 hits (same command as above). Alias chains themselves are safely capped at depth 8 (:522-524), so no unbounded worst case there.
Cost if unfixed: build-time only; a developer's typo in any glyph import, today on the site via `site/app/icons.vyrn:31`.
Smallest fix: stop scanning once a distance-1 name is found, or skip names whose length differs from `want` by more than a few. RECOMMENDATION, NOT A DECISION.

### 26. Syscall frequency — MEDIUM

What: every generated import site reads and parses the whole collection file again — T tags cost T reads plus T parses.
Where: `std/icons.vyrn:134` (the read inside `iconsAt`), reached once per emitted import because `iconProviderModule` emits one `iconsAt(...)` import per tag (:792-800).
Evidence: structure proven by the call chain above; end-to-end duplication NOT MEASURED — a scratch consumer reading a relative-path collection outside the repo sandbox hung past 120 s (`compiler/target/release/vyrn run <scratch>` reading `big.json` from the scratch directory), so the read-per-tag delta could not be timed. Parse-per-call cost at realistic size also bounded from below only: a 310 kB synthetic collection parsed under `VYRN_NO_GEN_CACHE=1 compiler/target/release/vyrn run` did not finish within 600 s, consistent with RFC-0107's measurement that generation-sandbox `parseJson` is quadratic in document size and lucide's 566 kB is unusable interpreted (`rfcs/RFC-0107-a-template-component-is-a-library.md:720-723,768-769`; the site's own collections are 22 kB and 23 kB).
Cost if unfixed: the site build reads and parses its pinned collections once per `<Icon>` tag — six tags today (`rfcs/RFC-0107-a-template-component-is-a-library.md:805`) — and the cost grows linearly with both tag count and collection size.
Smallest fix: memoize the readFile-plus-parse per collection path for the duration of one generation so sibling `iconsAt` imports share one parse. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 27, 28, 29, 30.
