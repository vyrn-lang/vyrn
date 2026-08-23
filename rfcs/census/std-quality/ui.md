# std/ui.vyrn

Lines: 3082. Exports: 32. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller points `pages(dir)` at a directory of page modules and imports the synthesized router: `route(req) -> Response`, typed-URL helpers, and a `routes()` table (std/ui.vyrn:2597). The module also carries the runtime half every router and page shares — the `Head` builder (`noHead`/`with*`/`headHtml`, lines 148-212), the query/lazy wrappers (`query`, `lazy`, `runQuery`, lines 255-298), the JSON data-channel payload builders (lines 444-488), the `PageError` type, and the closed `Page` contract pages are checked against (line 325). Site builds pay both halves: `site/export.vyrn:29` imports `route` from `pages("./app/routes")`, and `site/app/nav.vyrn:25-28` builds every page head through the combinators.

## Findings

### 8. Allocation frequency — MEDIUM

What: every `with*` head combinator rebuilds the whole `Head` record, copying all four arrays plus the title `String` to append one element.
Where: `std/ui.vyrn:154`, `161`, `168`, `175`, `182`.
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/ui/t1.vyrn` (single `withTitle`) min 72 ns; `...t22.vyrn` (title then meta) min 299 ns; `...t13.vyrn` (stylesheet twice) min 316 ns; `...t15.vyrn` (meta twice) min 427 ns; a direct literal with all 16 entries built once (`...t2.vyrn`) min 91 ns — so one combinator step costs more than an entire 16-entry record literal, and the cost grows with each step because each step re-copies arrays that already exist. Scratch files import `noHead`/`withTitle`/`withMeta`/`withStylesheet`/`withScript` from `std/ui`.
Cost if unfixed: `site/app/nav.vyrn:25-28` runs three combinators per rendered page head and `site/app/backstage.vyrn:169-176` runs five, on every render of every page in the site export.
Smallest fix: add in-place `push*` mutators alongside the copy-on-write combinators, or build the four arrays first and construct `Head` once. RECOMMENDATION, NOT A DECISION.

### 15. Best/worst/average case — MEDIUM

What: benchmarking any chain of three or more chained head combinators aborts the bench binary outright (exit code 116, no output), while chains of one or two measure fine and the identical three-combinator chain prints correctly under `vyrn run`.
Where: `std/ui.vyrn:153-183` (the combinator shape: `let mut ss = h.stylesheets` moves the field out, then later arms read `h.title.copy()`/`h.stylesheets.copy()` from the same parameter).
Evidence: `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/ui/t8.vyrn` (title → meta → stylesheet) exits 116 with no output, reproduced across five runs; `...t18.vyrn` (title → meta → meta) and `...t23.vyrn` (three metas) exit 116; `...t1.vyrn`, `...t11.vyrn`, `...t12.vyrn`, `...t15.vyrn` (all ≤2 combinators) print timings. The same three-chain as a `main` program (`vyrn run ... t20.vyrn`) prints `t` normally. Root cause NOT MEASURED — it may be a native-codegen or bench-harness bug triggered by this move-then-read field pattern rather than a std/ui logic error, but the pattern is this module's and every caller inherits it.
Cost if unfixed: anyone benchmarking a real site head (`site/app/nav.vyrn:26-28` is exactly a three-combinator chain) gets a silent abort instead of a number, and the latent memory question stays open for every generated router that ships these paths.
Smallest fix: rewrite each combinator to not read fields after moving one out (copy-first, or take `consume h`). RECOMMENDATION, NOT A DECISION.

### 26. Syscall frequency — LOW

What: every subdirectory of the page tree is listed twice during a scan — once by the `uiIsDir` probe, once by the recursive scan itself.
Where: `std/ui.vyrn:1515-1520` (`uiIsDir` calls `listDir` only to test directory-ness), consumed at `std/ui.vyrn:1560-1561` where `uiScanAll(sub, …)` immediately lists `sub` again.
Evidence: NOT MEASURED — both fns are `gen fn`s over generation-only `listDir` and cannot run under `vyrn bench`; the two-calls-per-directory path is provable from the cited lines.
Cost if unfixed: `pages("./app/routes")` at `site/export.vyrn:29` and the two `pagesThemed` scans at `bin/server/server.vyrn:19-20` double their directory-listing syscalls on every compile of the site and servers.
Smallest fix: drop `uiIsDir`; call `uiReadDirOf(sub)` directly and treat its `err != ""` case as "not a directory". RECOMMENDATION, NOT A DECISION.

### 2. Algorithm complexity — LOW

What: the collision checks compare every page against every other page, rebuilding helper-name and collision-key strings for both members of each pair, and the layout chain re-walks all layouts once per candidate depth.
Where: `std/ui.vyrn:1645-1657` and `1672-1686` (O(P²) pairwise loops; each iteration calls `uiHelperNameOf`/`uiCollisionKey` afresh, themselves segment walks building strings, lines 747-783); `std/ui.vyrn:1609-1619` (O(L × maxDepth) nested `want`/`i` loops inside `uiLayoutChain`, which `uiBuildModule` then runs twice per page, lines 2407 and 2478).
Evidence: O(P²) and O(L·maxDepth) follow from the cited loop bounds; wall-clock cost NOT MEASURED (generation-only code). A bench of the string-accumulation idiom used nearby showed near-linear behavior, so no quadratic claim is made there: `vyrn bench .../t3.vyrn` min 266 ns for 64 accumulated pieces, `...t24.vyrn` min 625 ns for 256 and 1.38 µs for 1024.
Cost if unfixed: compile-time only; `uiCollisionErrors` runs once per `pages` call (line 2613) on page counts in the tens today.
Smallest fix: precompute each page's helper name and collision key once into the scan result before the pairwise loops. RECOMMENDATION, NOT A DECISION.

### 21. Footprint size — LOW
What: every generated router embeds its own copy of the fixed runtime block, and the client bundle re-embeds byte-equivalent slice/split helpers under different names, so the same splitter exists three times in-tree.
Where: emitted router runtime `std/ui.vyrn:1707-1767` (~61 lines per router, plus optional head/error runtimes at 1773-1811); client copies `uiClientSlice` at 2714-2725 and `uiClientSegments` at 2726-2754 duplicate `uiRouteSlice`/`uiRouteSegments` at 1707-1747 and the module's own `uiSliceStr` at 497-505.
Evidence: line counts above from the file at the pinned commit; binary-size impact NOT MEASURED.
Cost if unfixed: each of `bin/server/server.vyrn:19-20`'s two routers and each client bundle carries redundant source; a fix to the splitter must be made in three places.
Smallest fix: emit a shared `std/ui:routeSegments(path)` import in place of the per-bundle copies. RECOMMENDATION, NOT A DECISION.

### 30. Determinism — LOW

What: within a dynamic-count class the emitted dispatch order, typed-URL helper order, and diagnostic order follow the OS directory enumeration order, so the synthesized router's text can vary between filesystems even though its behavior cannot.
Where: `std/ui.vyrn:2569-2588` (`uiDispatchOrder` sorts only by dynamic-segment count; ties keep scan order from `uiScanAll`, lines 1542-1574), propagated to emission at 2504-2506 and diagnostics at 1645-1657.
Evidence: order dependence follows from the cited loops; cross-platform variance NOT MEASURED.
Cost if unfixed: regenerated-router diffs churn for callers like `site/export.vyrn:29` when directory order changes, obscuring real changes.
Smallest fix: break ties in `uiDispatchOrder` by segment text. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 22, 23, 24, 25, 27, 28, 29.
