# std/vyx.vyrn

Lines: 5261. Exports: 32. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

Export kinds: 27 functions (`export fn` / `export gen fn`), 4 types (`VyxAttr`, `VyxNode`, `VyxBody`, `VyxTemplate`), 1 contract (`Component`, `std/vyx.vyrn:170`).

## What this module is for

A caller imports `components(dir)` (or `vyxPage` / `vyxLayout` / `vyxError`) at compile time. The module reads every `.vyx` file in `dir`, parses its `<script>` and `<template>` sections, and synthesizes one Vyrn module exporting one pure view function per component. `std/ui` mounts each `.vyx` route through the same machinery (`std/ui.vyrn:2275` emits `import * as p<n> from vyxPage(...)`); the example apps reach it through `components` and `componentsThemed` (`examples/bin/client/boot.vyrn:8`, `examples/fullstack/client/boot.vyrn:21`, `examples/shelf/client/boot.vyrn:13`). All of its work is generation-time; none of it runs at application runtime.

## Findings

### 2. Algorithm complexity — MEDIUM

What: One component compile lexes the whole `<script>` section at least three times.
Where: `std/vyx.vyrn:1301`.
Evidence: `vyxImportsFirstViolation` calls `vyxLexKwBlock`, which runs `lex(vyxSlice(ba, 0, ba.length))` over the whole section (`std/vyx.vyrn:1377`, `std/vyx.vyrn:1301`). `vyxParseScriptAt` then calls `vyxFindPropsBlock(sub)` on the same section, which reaches the same `lex` again (`std/vyx.vyrn:1428` → `std/vyx.vyrn:1330` → `std/vyx.vyrn:1301`). The duplicate-block check `vyxFindPropsBlockFrom` copies the remaining tail into a fresh array byte by byte (`std/vyx.vyrn:1518-1523`) and lexes it a third time (`std/vyx.vyrn:1524`). Each pass walks every token of the section (`std/vyx.vyrn:1304-1326`). Isolated cost of one `lex` pass: NOT MEASURED (the builtin is compiler-side).
Cost if unfixed: Every `.vyx` page, layout, error page, and component directory pays three lexer passes per `<script>` on every build; `std/ui`'s emitted router imports one `vyxPage(...)` generation per route (`std/ui.vyrn:2275`), and `examples/bin/client/boot.vyrn:8` compiles a whole widgets directory per client bundle.
Smallest fix: Lex the section once per compile and thread the token slice through `vyxImportsFirstViolation`, the props search, and the duplicate check. RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — LOW

What: Dead-helper stripping for client bundles is a fixed-point loop whose each round re-scans the whole script per candidate helper, with a substring search that restarts at every offset.
Where: `std/vyx.vyrn:3956`.
Evidence: `vyxStripDeadHelpers` runs up to 16 rounds (`std/vyx.vyrn:3959`). Each round's `vyxStripDeadHelpersOnce` walks every line, brace-matches the candidate body (`std/vyx.vyrn:3984`), and calls `vyxMentionsIdent` on the remaining script (`std/vyx.vyrn:3986`). `vyxMentionsIdent` calls `vyxFind(cb, nb, i)` from every surviving offset (`std/vyx.vyrn:3924-3938`), so one mention test is worst-case O(n·m) in script bytes n and needle length m, and one round is O(helpers · n²). Worst case overall: O(rounds · helpers · n²). No bench (gen-only path); bound proven by the loops cited.
Cost if unfixed: A client-bundle build of a page with many private helpers (`examples/bin/client/boot.vyrn:8` compiles the whole `app/routes/` tree this way) pays this per page.
Smallest fix: Collect helper names and their body ranges in one pass, remove all dead ones per round instead of one, and cap rounds by convergence rather than the constant 16. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — MEDIUM

What: The markup section scanner allocates fresh byte arrays inside its per-byte loop.
Where: `std/vyx.vyrn:465`.
Evidence: `vyxScanFindMarkup` tests every `<` with `vyxMatchAt(ba, bytes("<!--"), i)` and, on a comment, searches with `vyxFind(ba, bytes("-->"), i + 4)` (`std/vyx.vyrn:465-466`). Both `bytes()` calls construct a new `Array<UInt8>` each time, so a template with k `<` characters allocates at least k arrays per section scan, plus two per comment. The same pattern repeats across the module: `vyxSlice` copies its output one `push` per byte (`std/vyx.vyrn:183-185`) and backs every substring extraction in the parser. Isolated allocator counts: NOT MEASURED (comptime execution has no counter surface).
Cost if unfixed: Every build that compiles a `.vyx` file pays this during section extraction; `std/ui.vyrn:2275` triggers it once per route, and `examples/shelf/client/boot.vyrn:13` once per widget directory.
Smallest fix: Hoist `bytes("<!--")` and `bytes("-->")` to locals beside `close` and `openLead` (`std/vyx.vyrn:458-459`), which already do this. RECOMMENDATION, NOT A DECISION.

### 21. Footprint size — LOW

What: This is the largest module in `std/`, half again the size of the next.
Where: `std/vyx.vyrn:1`.
Evidence: `wc -l std/*.vyrn | sort -rn`: `std/vyx.vyrn` 5261, next `std/ui.vyrn` 3082, `std/graphql.vyrn` 2556; `std/` totals 30296 lines, so this file holds about 17 percent of `std/`. One file carries four distinct compilers (component, page, layout, error page) plus their shared scanners.
Cost if unfixed: Readers and reviewers face one 5261-line unit; the doc surface (`docs/api/std/vyx.md`) mirrors it.
Smallest fix: Split the byte-level scanners and the four module builders into internal files under one facade import. RECOMMENDATION, NOT A DECISION.

### 28. Initialization overhead — LOW

What: Generation cost grows close to linearly with component count, at roughly 9 ms marginal per component at 40 components.
Where: `std/vyx.vyrn:3019`.
Evidence: Cold `compiler/target/release/vyrn emit-gen <main>` over generated scratch projects with identical components: 1 component 96 ms, 20 components 195 ms, 40 components 378 ms (marginal (378−96)/39 ≈ 7 ms per component; the 20→40 doubling costs 1.94x, near linear). Warm-cache `vyrn check` on the same projects: 86 ms, 100 ms, 124 ms. Pure exported helpers are cheap at runtime: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/vyx/b.vyrn` reports `bench "query data type ParamQuery<Result>" min 513 ns median 707 ns mean 790 ns (614 samples × 1024 iters)` for `vyxQueryDataType`.
Cost if unfixed: Large component directories add build time in seconds-scale steps today; `examples/bin/client/boot.vyrn:8` compiles a full widgets directory into every client bundle.
Smallest fix: Nothing urgent; keep the per-component pipeline linear when touching the scanners. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 22, 23, 24, 25, 26, 27, 29, 30.
