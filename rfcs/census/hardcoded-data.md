# Census A7 — everything hardcoded that should be data

Date: 2026-08-23. Branch `main`. Survey only; nothing was fixed.

Method: ten independent surveys, one per area, run in parallel. Each survey
formed its conclusions alone. Every row carries a `path:LINE` citation. Three
claims were spot-checked against the files before publication:
`site/export.vyrn:377`, `site/app/meta.vyrn:68`, `docs/releasing.md:34` and
`:45`. All matched.

## Counts

| area | findings | SILENT | LOUD |
| --- | --- | --- | --- |
| `compiler/**` | 38 | 32 | 6 |
| `.github/workflows/*.yml` (+ release docs, install scripts) | 11 | 11 | 0 |
| `site/app/routes/**` | 12 | 12 | 0 |
| `site/app/*.vyrn` | 10 | 6 | 4 |
| `site/export.vyrn` | 10 | 7 | 3 |
| `rfcs/README.md` | 6 | 4 | 2 |
| `site/app/nav.vyrn` | 4 | 2 | 2 |
| `site/public/style.css` | 4 | 3 | 1 |
| `std/tw.vyrn` | 4 | 3 | 1 |
| `std/icons.vyrn` | 3 | 1 | 2 |
| **total** | **102** | **81** | **21** |

`LOUD` means an existing test or gate catches the drift. `SILENT` means the
wrong value ships. Four fifths of this census ships wrong before anyone
notices.

### The reserved-word memory, corrected

A project memory records 83 reserved names across roughly 45 lists. Counting
gives different numbers:

- Strictly reserved: **107 distinct names** = 24 lexer keywords +
  73 `checker::RESERVED` entries (`compiler/vyrn-frontend/src/checker.rs:200-296`)
  + 11 `MOVED_TO_STD` entries (`checker.rs:311-323`), minus `match`, which sits
  in both the keywords and `RESERVED`.
- Broader union (method-form builtins, code-quote builtins, constructor
  aliases, contextual grammar words): about **137 names**.
- Enumerated list locations: **38**, not ~45. All 38 are rows in the table
  below.

Drift is not hypothetical. It is live today:

- `logging` and `from` sit in the editor keyword regex
  (`editor/vscode/vyrn.tmLanguage.json:134-137`). Neither is a lexer keyword
  (`compiler/vyrn-frontend/src/lexer.rs:236-261`; `from` is contextual only,
  `parser.rs:2357`).
- `lane`, `replaceLane`, `anyTrue`, `allTrue` exist in parser
  `METHOD_BUILTINS` (`compiler/vyrn-frontend/src/parser.rs:133-139`) but are
  absent from the LSP hover table (`compiler/vyrn-frontend/src/symbols.rs:3747-3774`).
  Vector methods get no hover and no completion.

## Findings

Full table, `SILENT` first. Within each risk class, rows are grouped by area.
`read` is how the value should be obtained: `gen fn` = Vyrn gen fn at compile
time (a Vyrn generator can `readFile` and `listDir` at compile time — most
rows want exactly this), `readFile` = read at build time, `derived` = computed
from another value, `generated` = generated file, committed.

### SILENT

| what | where | why it will go stale | single source | read | risk |
| --- | --- | --- | --- | --- | --- |
| Lexer `Tok` keyword variants (24) | `compiler/vyrn-frontend/src/lexer.rs:31-55` | two spelling tables below must track every variant | proposed `compiler/vyrn-frontend/src/keyword_table.rs` used by all three tables | derived | SILENT |
| `keyword_or_ident` spelling match (24 strings) | `compiler/vyrn-frontend/src/lexer.rs:236-261` | new keyword without an arm lexes as `Ident`, silently | the `Tok` enum | generated | SILENT |
| `token_name_and_text` `kw()` arms re-spell all 24 | `compiler/vyrn-frontend/src/lexer.rs:131-154` | public keyword spellings go stale if enum and arm diverge | the `Tok` enum | derived | SILENT |
| VON field-name keyword blacklist, 24 strings re-typed in Vyrn | `std/von.vyrn:1127-1128` | a keyword added in Rust leaves this list accepting bad field names; currently byte-identical to the lexer's 24 | ask the `lex()` builtin, as `std/vyx.vyrn` does | derived | SILENT |
| vscode tmLanguage keyword regex (26 alternatives) | `editor/vscode/vyrn.tmLanguage.json:134-137` | contains stale `from` and `logging` right now | generated keyword list from the lexer | generated | SILENT |
| vscode contextual-word patterns | `editor/vscode/vyrn.tmLanguage.json:66-78,118-133` | each new contextual word needs a pattern or is miscoloured | generated contextual-word list mirroring parser positions | generated | SILENT |
| `MOVED_TO_STD` migration table (11 removed builtins) | `compiler/vyrn-frontend/src/checker.rs:311-323` | re-entry into `RESERVED` breaks imports | std module export tables | derived | SILENT |
| LSP `MACRO_BUILTINS` semantic-token list (27 names) | `compiler/vyrn-frontend/src/symbols.rs:3162-3195` | comment says kept-in-sync by hand; next free builtin ships uncoloured | filter of `checker::RESERVED` | derived | SILENT |
| floor `CALLS` capability pairs (7 IO builtins) | `compiler/vyrn-frontend/src/floor.rs:132-140` | new filesystem builtin without a row ships with no capability floor | effect annotations beside prelude seeded signatures | derived | SILENT |
| Loader builtin alias exports `Result Ok Err Option Some None` | `compiler/vyrn-frontend/src/loader.rs:381-386` | rename splits import validation from completions | this table, others consume it | derived | SILENT |
| `CONSTRUCTOR_BUILTINS Some None Ok Err` | `compiler/vyrn-frontend/src/symbols.rs:3198` | third copy of the constructor names | `loader.rs:381-386` | derived | SILENT |
| Completion loop inline array `Result Ok Err Option Some None` | `compiler/vyrn-frontend/src/symbols.rs:1091` | inline literal beside two other copies of the same six | `loader.rs:381-386` | derived | SILENT |
| LSP `ALL_BUILTIN_METHODS` hover table (24 rows) | `compiler/vyrn-frontend/src/symbols.rs:3747-3774` | drift live: `lane replaceLane anyTrue allTrue` missing | `parser::METHOD_BUILTINS` joined with detail prose | derived | SILENT |
| `log_level_ordinal` name-to-level map | `compiler/vyrn-frontend/src/ast.rs:143-151` | adding a level touches six sites | shared log-level table | derived | SILENT |
| Checker log-level dispatch `matches!` | `compiler/vyrn-frontend/src/checker.rs:6243` | sixth level skips arity checking silently | `ast.rs:143-151` keys | derived | SILENT |
| Interpreter log-level dispatch arm | `compiler/vyrn-frontend/src/interp.rs:5066` | new level falls through to unknown-call error | `ast.rs:143-151` | derived | SILENT |
| Direct backend result-type arm (print + log five + logger) | `compiler/vyrn-codegen/src/direct.rs:5292-5293` | missing entry mistypes the result and miscompiles | `ast.rs:143-151` | derived | SILENT |
| Direct backend lowering arm | `compiler/vyrn-codegen/src/direct.rs:6493` | new level emits no wasm log write | `ast.rs:143-151` | derived | SILENT |
| Textual LLVM emitter guard | `compiler/vyrn-codegen/src/lib.rs:9530` | third backend copy of the same five names | `ast.rs:143-151` | derived | SILENT |
| Checker surface-builtin match + diagnostic arms (`render rawAt raw lex`) | `compiler/vyrn-frontend/src/checker.rs:6449-6458` | shadowing rule re-spells the four names | proposed surface-builtin table shared by frontend and backends | derived | SILENT |
| Interpreter shadowed check re-lists the four | `compiler/vyrn-frontend/src/interp.rs:4719-4721` | fifth surface builtin stays unshadowable and hijacks user functions | same table as `checker.rs:6449` | derived | SILENT |
| `CODE_IMPORTS` extern names mirrored by `direct.rs` `GenExterns` | `compiler/vyrn-codegen/src/lib.rs:973-978`; `compiler/vyrn-codegen/src/direct.rs:183-190` | two backends enumerate the same ABI names apart | one shared extern-name table | derived | SILENT |
| genwasm host functions `rawAt`/`render` link names | `compiler/vyrn-genwasm/src/lib.rs:1486,1541` | third copy of the code-quote surface across crates | `CODE_IMPORTS` at `lib.rs:973-978` | derived | SILENT |
| LSP `DECL` hover-prefix declaration forms | `compiler/vyrn-lsp/src/main.rs:782-792` | new declaration form gets unfenced hover docs | declaration forms in `ast.rs` | derived | SILENT |
| `RPC_GENERATORS` (five) vs `MAP_GENERATORS` (six) | `compiler/vyrn-lsp/src/main.rs:3121-3127`; `main.rs:3134-3141` | five names duplicated verbatim one screen apart; divergence loses hovers silently | one superset table, other derived by filter | derived | SILENT |
| `DIRECTIVES` template directive vocabulary (eight) | `compiler/vyrn-lsp/src/templates.rs:319-328` | directive added in `std/vyx` never reaches completion | std/vyx directive grammar | derived | SILENT |
| `GLOBAL_ATTRS` HTML attribute vocabulary (sixteen) | `compiler/vyrn-lsp/src/templates.rs:330-347` | hand vocabulary; HTML growth passes it by | std/html attribute definitions | derived | SILENT |
| Playground `CONTEXTUAL` list (ten words) | `compiler/vyrn-play/src/lib.rs:157-159` | comment says "the same list as site/app/hl.vyrn"; twin copy, no test compares them | shared generated `contextual.json` | generated | SILENT |
| Site highlighter `contextual()`, same ten words | `site/app/hl.vyrn:184-186` | twin of the playground copy; grammar change recolours one surface only | shared generated `contextual.json` | generated | SILENT |
| genwasm `UNSERVED` storage builtins | `compiler/vyrn-genwasm/src/lib.rs:62` | new unservable builtin keeps serving and hangs the guest (`writeAtomic` already absent from floor `CALLS`) | storage capability rows in frontend floor | derived | SILENT |
| `HOST_EXTERNS` ABI spellings re-typed in three more sites | `compiler/vyrn-codegen/src/direct.rs:7382-7384`; `compiler/vyrn-codegen/src/lib.rs:1386-1388`; `toolchain.rs:393` | ABI string lives in four files; canonical table owns the Vyrn-name-to-symbol half | `trap::HOST_EXTERNS` at `compiler/vyrn-frontend/src/trap.rs:215-219` | derived | SILENT |
| Generated-module reserved names in `std/icons.vyrn` | `std/icons.vyrn:447-449` | std/html type renames leave collisions undetected | std/html exports plus generator names | derived | SILENT |
| Generated-export reserved names in `std/i18n.vyrn` | `std/i18n.vyrn:969` | exports renamed elsewhere in the same file leave collisions undetected | i18n.vyrn's own export statements | derived | SILENT |
| GitHub org/repo URLs re-typed beside `github.vyrn` | `site/app/backstage.vyrn:162,201`; `site/app/feed.vyrn:76`; `site/app/packages.vyrn:449` | repo move breaks four shipped links while `site/app/github.vyrn:19` owns the name | `repoName()`/`ghBlob()`/`ghRelease()` in `site/app/github.vyrn:8-14` | derived | SILENT |
| Pinned-install fallback invents version v0.1.0 | `site/app/routes/install.vyx:57` | names a version nothing published; dead branch keeps lying as tags move past it | `releaseTag()` via `site/release.txt` (`site/app/repo.vyrn:101-103`) | derived | SILENT |
| Build-from-source command types clone URL | `site/app/routes/install.vyx:40` | repo rename breaks a copy-pasted command; the name is already spelled once in `site/app/github.vyrn:8` | `repoName()`, proposed `ghClone()` beside `ghBlob` | derived | SILENT |
| Two blob URLs for install scripts typed inline | `site/app/routes/install.vyx:142` | third and fourth spelling of the repo name on one page; `ghBlob` builds exactly these URLs | `ghBlob(path)` at `site/app/github.vyrn:12-14` | derived | SILENT |
| Footer link hardcodes repository URL | `site/app/routes/layout.vyx:160` | third file spelling `vyrn-lang/vyrn` | `repoName()` at `site/app/github.vyrn:8` | derived | SILENT |
| Playground tooltip retypes timeout, depth limit, recursion limit | `site/app/routes/play.vyx:128`; `site/public/play.js:20`; `site/public/play-worker.js:37-38,48`; `compiler/vyrn-frontend/src/interp.rs:42` | four copies of three limits across JS and Rust; raising any limit leaves the tooltip asserting stale numbers | proposed `site/data/playlimits.json` written by the wasm build step, or compile-time `readFile` of the sources | readFile | SILENT |
| Radar plate eyebrow types dataset filename | `site/app/routes/benchmarks.vyx:81`; `site/app/bench.vyrn:98-99` | repointing `bench.vyrn` at a new record leaves the plate crediting the old file | export `run1File()`/`run2File()` from `app/bench.vyrn` and interpolate | derived | SILENT |
| Headings type "Eight programs, five engines" | `site/app/routes/benchmarks.vyx:48`; `site/app/routes/index.vyx:304,328` | ninth program or sixth contestant leaves three sentences wrong on two pages | `programs().length` / `contestants().length` (`site/app/bench.vyrn:192,238`) interpolated | derived | SILENT |
| Guide lede types "Eleven chapters" | `site/app/routes/guide/index.vyx:54` | twelfth chapter ships and the landing understates the book | `chapterCount("guide")`, which already exists (`site/app/guide.vyrn:712-714`) | derived | SILENT |
| Web shelf lede types "Four pages" | `site/app/routes/web/index.vyx:49` | the list under the sentence is generated; the sentence is typed, so they can disagree | sum over `areaGroups("web")` in the same file (`web/index.vyx:24-27,57`) | derived | SILENT |
| Editor lede types "about five hundred lines" | `site/app/routes/tooling/editors.vyx:66` | any edit to `editor/vscode/extension.js` moves the number; nothing recomputes it | proposed gen fn counting lines of `extension.js` at compile time | gen fn | SILENT |
| Non-goals list retyped with reasons on Why Vyrn | `site/app/routes/why-vyrn.vyx:42-50`; `README.md:318-322`; `rfcs/RFC-0001-vision.md:103-115` | three lists in three places; an RFC decision never reaches the page without a hand edit | RFC-0001 Non-goals section | readFile | SILENT |
| Search-index size "42 KB" asserted in three comments | `site/app/routes/layout.vyx:102`; `site/app/search.vyrn:5,35-36` | index grows with every row; no test pins the comments | `indexJson().byteLength` computed at build (`site/app/search.vyrn:176`) | derived | SILENT |
| Three-engine benchmark readings typed in | `site/app/chart.vyrn:499-501` | next `sh bench/engines.sh` run replaces the readings; bars go wrong quietly | proposed results record beside `bench/engines.sh`, read like `bench.vyrn:99` does | readFile | SILENT |
| Engine caption pins vyrn 0.1.0-alpha.1, wasmtime 46.0.1, 14 August 2026 | `site/app/chart.vyrn:517` (comment restates at 487-488) | next release and next benchmark run both stale it | same record as the readings row above | readFile | SILENT |
| Benchmarks-game ceiling times for eight programs typed in | `site/app/bench.vyrn:1430-1437` | game republishes times; page quotes them with no fetch; times unpinned by tests | proposed snapshot, e.g. `site/data/ceiling.json` written when fetched | readFile | SILENT |
| Fetch-date caption hardcodes "read 2026-08-19" | `site/app/bench.vyrn:1444` | later fetch without editing this string lies about freshness | derive from the ceiling snapshot's own fetch date | derived | SILENT |
| Deviation-causes prose quotes ms, ratios, MB from current record | `site/app/bench.vyrn:1196-1203,1214-1221` | replacing the results JSON updates tables but leaves this prose contradicting them | the `rfcs/bench-0104/results` record fields, or pin prose to a record version | readFile | SILENT |
| Language tagline maintained in three places, wording already diverged | `site/app/nav.vyrn:28`; `site/app/meta.vyrn:29`; `README.md:5-8` | next edit diverges again, silently | `metaOf("/")` blurb (`site/app/meta.vyrn:29`) or proposed `site/data/tagline.md` | derived | SILENT |
| Nav labels duplicate page-title prefixes; Play vs Playground already diverged | `site/app/nav.vyrn:160,164,165`; `site/app/meta.vyrn:30,35,37`; `site/app/search.vyrn:45,49,55,196` | labels restated across meta table and search sections; `Playground` vs `Play` proves drift happens | PageMeta table at `site/app/meta.vyrn:28-41` | derived | SILENT |
| `--field` RGB triplets duplicate `--navy`/`--bright` tokens as raw components | `site/public/style.css:48,257,282` | retuning a token leaves all three copies stale; nothing reads `--field` today, so a future consumer ships the wrong colour | the token blocks themselves (`style.css:24-39`) | derived | SILENT |
| Breakpoint values repeated inline: 6 px values across 13 queries | `site/public/style.css:596,616,649,1294,1320,1487,1588,1688,1756,1836,2306,2651,2693,3000,3182` | min-width and max-width pairs must stay complementary; moving one boundary touches six queries and a missed copy splits layouts with no error | proposed `site/data/breakpoints.json` stamped into the sheet by the export generator | gen fn | SILENT |
| z-index ladder hand-enumerated: 1/30/40/60/60/70/100/200 | `site/public/style.css:894,858,3162,2729,3169,3177,424,631` | each value is chosen relative to all others; equal levels stack by source order and ship wrong with no gate | proposed `--z-*` custom properties declared once in `:root` | derived | SILENT |
| Claim that the pinned icon collections hold 180 glyphs | `site/app/routes/install.vyx:17`; sibling claim `site/test/icons.test.mjs:6` | bumping the pin at `vyrn.json:9-10` changes both counts | the pinned collection JSON itself | derived | SILENT |
| State-prefix set `{hover:, focus:}` listed twice with divergent shapes | `std/tw.vyrn:691-698,772,1030` | adding `active:` to one site makes the token regex and emitted CSS disagree silently | one ordered state table inside `std/tw.vyrn` | derived | SILENT |
| State-layer count 3 hard-coded beside `twStateLayer`'s 3 branches | `std/tw.vyrn:737`; `std/tw.vyrn:691-698` | fourth state breaks flat-rule indexing silently | length of the same state table | derived | SILENT |
| Sample theme hex values duplicated across two files | `std/tw.vyrn:31-38`; `std/von.vyrn:1414-1419` | both illustrate the same fictional palette; editing one strands the other | shared example fixture, or accept as prose | readFile | SILENT |
| Feed `<title>` typed twice: head stamp and RSS channel | `site/export.vyrn:361`; `site/app/feed.vyrn:52` | renaming the feed in one place leaves every page advertising a different feed name than the channel | proposed exported `feedTitle()` in `site/app/feed.vyrn` | derived | SILENT |
| Asset list: 12 hand-typed source-to-published pairs | `site/export.vyrn:562-579`; published names restated in `site/app/routes/layout.vyx` head block and `site/export.vyrn:353` | a script added to a layout but forgotten here ships a page pointing at a file no route wrote; existing names are gated, a new one is not | scan rendered documents' href/src attributes, or an assets manifest beside `site/public/` | derived | SILENT |
| Route strings re-typed in `currentNav()`/`subnavKey()` despite imported helpers | `site/export.vyrn:155,156,158,169,178,183,186,264-279` | new shelf under `/docs` gets no masthead mark and no subnav band; nothing asserts the two agree | the path helpers already imported at `site/export.vyrn:29` | gen fn | SILENT |
| "ten consumer pages" stated five times while the table holds 13 rows | `site/export.vyrn:377,1308,1336`; `site/app/meta.vyrn:14,26` vs the passing `assertEq(all.length, 13)` at `site/app/meta.vyrn:68` | already stale — M4/M5 grew the table and the prose never followed | `pageMetas().length` interpolated | derived | SILENT |
| `readFile("site/public/style.css")` re-types a path `assets()` owns | `site/export.vyrn:1278` vs `site/export.vyrn:562` | if the stylesheet moves, the Err arm yields "" and the real-file assertions pass vacuously behind a `> 0` guard | look the source up in `assets()` | derived | SILENT |
| mkdir directory list written in a header comment, owned by the deploy workflow | `site/export.vyrn:25`; executed by `.github/workflows/site.yml` (comments at `site/export.vyrn:20-23`) | new nested area publishes nothing until someone edits YAML the type system never sees | derive required directories from `sitePaths()` at run time and fail naming the missing one | derived | SILENT |
| "/play.html" exception re-typed in three fragment gates | `site/export.vyrn:1430,1437,1460` | playground route move leaves the gates excluding a name no route answers, silently | `published(playPath())` via `site/app/nav.vyrn:96-105` | derived | SILENT |
| Hand-typed stalled-RFC list (15 numbers) | `rfcs/README.md:39-40` | restates each file's Status header; any milestone landing stales it; the index test skips status text (`compiler/vyrn-cli/tests/rfc_index.rs:33-47`) | the Status headers themselves | derived | SILENT |
| Status and Title columns of the index table hand-copy all 108 RFC headers | `rfcs/README.md:58-166` | membership and numbering are checked; copied status/title text is not, and drift ships | each linked file's Status header and title heading | derived | SILENT |
| Cost claim "renumbering thirty files" | `rfcs/README.md:49` | typed cross-reference count grows with the corpus; unchecked | derivable from the cross-reference scan in `compiler/vyrn-cli/tests/rfc_index.rs:269` | derived | SILENT |
| Counts inside other-docs blurbs: "ten phases", "four Vyrn redesigns" | `rfcs/README.md:176,181` | retells contents of linked documents; editing those documents stales the counts | the linked documents' headings | readFile | SILENT |
| checkout action SHA re-typed nine times | `.github/workflows/ci.yml:214,346,517,613,678`; `site.yml:75`; `release.yml:97,279,332` | one bump edits one site, leaves eight stale; the `@v4` comment above each use can lie after a bump | proposed `.github/actions/checkout` composite action | derived | SILENT |
| rust-cache SHA re-typed six times | `ci.yml:221,349,520,616,681`; `site.yml:106` | same divergence across five jobs in two files | proposed `.github/actions/rust-cache` composite | derived | SILENT |
| actions/cache SHA typed twice | `ci.yml:243,546` | two copies drift when one job bumps | proposed composite action | derived | SILENT |
| upload-artifact SHA typed three times | `ci.yml:740`; `release.yml:261,320` | three copies drift when one bumps | proposed composite action | derived | SILENT |
| Runner-label matrix duplicated within and across workflows | `ci.yml:203,340`; `release.yml:73,83,87,91` | test matrices must mirror the release matrix; a missed leg leaves a shipped platform untested, silently | `release.yml` build matrix as canonical | derived | SILENT |
| Archive/platform names typed in five files | `release.yml:74,84,88,92`; `install.sh:30,37,38`; `install.ps1:38`; `docs/releasing.md:48-51`; `install-test.sh:58` | adding a platform means five edits; `releasing.md:145` already names the wrong runner label (ubuntu-22.04-arm vs ubuntu-24.04-arm at `release.yml:83`) | `release.yml` matrix; generate the install-script cases from it | generated | SILENT |
| Archive contents list typed four times | `release.yml:165-169`; `docs/releasing.md:66-74`; `install.sh:124-137`; `install.ps1:111,124-127` | adding an archive member needs four edits; the install scripts' stale-file cleanup lists rot first | `release.yml` staging step; proposed committed dist manifest both stage and install read | readFile | SILENT |
| Repo slug and raw install URLs typed five times | `release.yml:367,371`; `install.sh:4,17`; `install.ps1:3,25`; `docs/releasing.md:106-107` | fork or rename breaks all five strings | `github.repository` event context in workflows; scripts keep `VYRN_REPO` as override | derived | SILENT |
| vsce@3.9.2 restated in workflow comments | `release.yml:289,305` vs `editor/vscode/package.json:14` | npm bump leaves the comments lying | delete the comment versions; the package script is the one spelling | derived | SILENT |

### LOUD

| what | where | why it will go stale | single source | read | risk |
| --- | --- | --- | --- | --- | --- |
| `checker::RESERVED` (73 names), plausibly the canonical table | `compiler/vyrn-frontend/src/checker.rs:200-296` | subsets below hand-copy it | itself; subsets should filter it | derived | LOUD — `cli/tests/reserved.rs` pins 5 of 73 names |
| `SPAWN_FORBIDDEN` (18-name subset) | `compiler/vyrn-frontend/src/checker.rs:9338-9361` | must track `RESERVED` | effect-bearing rows of `RESERVED` | derived | LOUD — test `spawn_forbidden_names_are_reserved` |
| `MOVED_TO_STD` honesty | `compiler/vyrn-frontend/src/checker.rs:308-311` | a moved name re-added to `RESERVED` breaks imports | std module export tables | derived | LOUD — test `every_moved_name_is_gone_from_reserved` |
| Parser `METHOD_BUILTINS` surface pairs (16) | `compiler/vyrn-frontend/src/parser.rs:103-140` | `ALL_BUILTIN_METHODS` must mirror surfaces; the named gate covers reservation only, not LSP sync | proposed single builtin registry | derived | LOUD — gate `every_method_builtin_is_reserved_or_shadowable` |
| `HOST_EXTERNS` mapping table | `compiler/vyrn-frontend/src/trap.rs:210-219` | deliberately the single table; its accessor functions guard the drift | itself | derived | LOUD — accessors panic on unknown key |
| CLI reserved-name regression sample (5 names) | `compiler/vyrn-cli/tests/reserved.rs:41` | only loud gate on `RESERVED` membership; the other 68 names ship silent | `checker::RESERVED` | readFile | LOUD — the named test |
| Nav rows restate route paths as literals | `site/app/nav.vyrn:160,164,165`; helpers at `site/export.vyrn:29,55` | route rename must be edited on both sides | `pages("./app/routes")` generated module | gen fn | LOUD — test "every navigation row points at a route that exists", `site/export.vyrn:897-906` |
| Install CTA repeats `/install` four times | `site/app/nav.vyrn:183`; `site/export.vyrn:155,156,930`; helper at `site/export.vyrn:29,55` | rename strands the CTA href and the `currentNav` special case | `installPath()` from the generated routes module | gen fn | LOUD — masthead test built at `site/export.vyrn:930` |
| Orphan-page classification re-types `/`, `/compare`, `/why-vyrn`, `/releases` | `site/export.vyrn:949`; documented at `site/app/nav.vyrn:113-121` | fourth independent spelling of "which routes left the nav row" | the imported path helpers plus `redirectTarget()` | gen fn | LOUD — assertion at `site/export.vyrn:950`, but only after hand-syncing four lists |
| Theme-button markup spelled out in a test literal | `site/export.vyrn:1016`; single source `site/app/nav.vyrn:87-88` | wording change to `themeControl()` breaks the test until the string is copied by hand | import `themeControl` (import exists at `site/export.vyrn:30`) | derived | LOUD — count drops to 0 on drift |
| Index size budget split across two files | `site/export.vyrn:816` (raw ceiling); gzipped ceiling in `site/test/search.test.mjs` (cited from the comment at `site/export.vyrn:806-813`) | one budget fact as two independently editable numbers | one constant in `site/app/search.vyrn`; the mjs reads the printed artifact header (`site/export.vyrn:821`) | generated | LOUD — each ceiling fails its own gate |
| Eight wasm module sizes typed as literals | `site/app/chart.vyrn:164-171` | any compiler change rebuilds those .wasm files; sizes drift on the next commit | proposed gen fn running `vyrn build --target wasm`, or `site/data/modulesizes.json` | gen fn | LOUD — chart tests pin bar values at `site/app/chart.vyrn:720-722` |
| clang route fixed at 277 KB, label typed twice | `site/app/chart.vyrn:230,251,252` | figure quoted from `README.md`/`ROADMAP.md`; a new fib.wasm build invalidates it | one shared constant parsed from the README figure | readFile | LOUD — test pins bars[0] "277 KB" at `site/app/chart.vyrn:765` |
| Leak-census readings 396/1573/5032 transcribed by hand | `site/app/chart.vyrn:414-416`; source `rfcs/census-call-arguments.md` Finding 2 | next census run republishes numbers; the chart keeps old ones | the census document | readFile | LOUD — tests pin "15.7 MB"/"50.3 MB" at `site/app/chart.vyrn:773-775` |
| Bench dataset filenames with date stamps typed twice | `site/app/bench.vyrn:99,103` | next record forces repointing two functions plus tests; one missed literal renders em-dashes | `rfcs/bench-0104/results/` directory listing instead of two literals | gen fn | LOUD — test pins benchDate "2026-08-19" at `site/app/bench.vyrn:1471` |
| Dark palette written twice, verbatim (~15 declarations x 2) | `site/public/style.css:246-265,271-290` | a selector list cannot hold a media query, so both copies are hand-typed; editing one strands system-dark readers | emit both copies from one dark-palette table in the sheet processor | gen fn | LOUD — `site/test/contrast.test.mjs:279` asserts token equality between the blocks |
| Provider prefix vocabulary re-typed beside the vyrn.json alias keys it mirrors | `site/app/icons.vyrn:31`; `vyrn.json:9-10` | add, rename or drop a dependency alias and this string drifts; unused stale entries stay silent | vyrn.json dependencies object; proposed provider gen fn reading its keys | gen fn | LOUD — unbound prefix fails the page build (`std/icons.vyrn:1188`) |
| Hand-typed roster of templates carrying Icon tags | `site/test/icons.test.mjs:57-66` | new template gaining tags must be added to two lists by hand | scan of `site/app/routes/*.vyx`, which the test already performs | readFile | LOUD — the file's first assertion names the missing template (`site/test/icons.test.mjs:69-79`) |
| Literal `[0, 3 * M)` flat-index contract documented independently of code | `std/tw.vyrn:701-708,737` | comment states the shape while one line computes it; prose/code drift misleads maintainers | multiplier derived from the shared state-table constant | derived | LOUD — `twRuleFlat` test cases at `std/tw.vyrn:1005-1011` |
| RFC count sentence "107 RFCs, numbered 0001 to 0108" | `rfcs/README.md:47` | next RFC makes the count and range wrong on its own | rfcs/ directory listing, already read by the gate | derived | LOUD — `compiler/vyrn-cli/tests/rfc_index.rs:203-228` |
| Gap claim "There is no RFC-0066" | `rfcs/README.md:47-50` | filling the gap invalidates the prose | directory listing via `gap_numbers()` | derived | LOUD — `compiler/vyrn-cli/tests/rfc_index.rs:242-256` |

## Already correct

Places that already derive a fact instead of typing it. These are the repair
patterns for the table above.

### Compiler

- Site snippet highlighter takes every keyword decision from the `lex()`
  builtin; there is no keyword list to drift (`site/app/hl.vyrn:12-13,315-317`).
- `std/apidoc` derives modules, functions and tests entirely from `lex()`
  tokens (`site/app/apidoc.vyrn:182-196`).
- `std/vyx` locates section keywords through the real lexer
  (`std/vyx.vyrn:1289-1300`).
- Movecheck view/sink facts were once hand lists; they are now derived from
  prelude seeded signatures and pinned by tests (`movecheck.rs:438-456,4647-49`;
  `prelude.rs:574-582`; `checker.rs:9337`).
- Formatter prints raw token text and needs no keyword table
  (`lexer.rs:198-213`).
- Loader import-path resolution is reused by the LSP instead of a second copy
  (`loader.rs:391-393`).
- `HOST_EXTERNS` is deliberately the one mapping table, with a comment naming
  the drift a second copy would cause (`trap.rs:210-219`).
- `std/vyx-hints.vyrn` holds no name tables, only rule codes.

### Site export and app

- Route list built from `pages("./app/routes")` generated typed URL helpers;
  renamed or deleted routes fail at compile time (`site/export.vyrn:29-67`).
- Redirect targets use the same generated helpers; the redirect count 6 is a
  deliberate tripwire, not a duplicated fact (`site/export.vyrn:96-107,962`).
- Featured release tag and date: baked `site/release.txt` read by the
  `repoFacts` gen fn, reconciled against `site/data/history.json`
  (`site/app/facts.vyrn:191`; `site/app/repo.vyrn:101-118,186-206`; export
  sweep test at `site/export.vyrn:1236-1260`).
- Crawler cards are read back out of each rendered document's own title and
  description, sourced from the one table `site/app/meta.vyrn:27-41`
  (`site/export.vyrn:365-433`).
- Site origin and install commands spelled once and composed
  (`site/app/repo.vyrn:153-180`; asserted at `editors.vyrn:245-249`).
- Feed items derive from `releaseRows()`; day and month names computed by
  Zeller's congruence, not tabulated (`site/app/feed.vyrn:24-90`).
- Relative depth recomputed from each page's published path; no base flag
  exists to go stale (`site/app/nav.vyrn:130-132`; `site/export.vyrn:488-493`).
- Search index built at export time from the same sources the pages render
  (`site/export.vyrn:814-823`).
- Benchmark medians, runs and date read from the committed results JSON
  (`site/app/bench.vyrn:171,1471-1472`).
- Commit, PR and release history read from `site/data/history.json` written by
  `scripts/site-history.py` (`site/app/history.vyrn:65-66`).
- API docs and dependency graph regenerated every build
  (`site/app/docs.vyrn:12`; `site/app/deps.vyrn:14`).
- Package registry cards read off `examples/*/vyrn.json`
  (`site/app/packages.vyrn:30-31,86,97,115`).
- Guide breadcrumbs and pagers come from the one chapter table
  (`site/app/guide.vyrn:711-714`; `site/app/routes/guide/[chapter].vyx:79`).
- Footer counts generated over `examples/` and backends
  (`site/app/routes/layout.vyx:148-150`; `site/app/repo.vyrn:18-30`).
- `sitePaths()` builds every published path through typed helpers, never
  strings (`site/export.vyrn:54-55`); `pageHeadOf` takes the path and reads
  title/blurb from the meta table (`site/app/nav.vyrn:51-56`);
  `redirects()` names old and new routes through typed helpers with both sides
  asserted (`site/export.vyrn:76-82,1000-1006`).

### Icons and Tailwind

- Icon glyph bytes come from hash-locked Iconify JSON via `readFile` at
  generation time; misspelled glyphs fail the build with a nearest-name
  suggestion (`std/icons.vyrn:112-113,134`; pin at `vyrn.json:9-10`).
- Per-tag resolution means no central glyph list exists
  (`std/icons.vyrn:679`; use at `layout.vyx:153,160`).
- Icon docs machine-generated with a drift gate
  (`compiler/vyrn-cli/src/main.rs:30-32`).
- `std/tw.vyrn` derives every scale axis from a theme file read at compile
  time: `readFile(theme)` (`std/tw.vyrn:868-869`), axes projected through
  `twAxisOf` (`:561-576,373-384`), colour/padding/gap/breakpoint loops at
  `:445-458,476-491,495-516,736-744`. Changing a design token means editing
  `theme.json` only.

### Workflows and docs

- Toolchain tool versions live only in `vyrn.json:3-6`, hash-locked in
  `vyrn.lock:3-12`; CI resolves them with `vyrn update --locked` and cache
  keys hash `vyrn.lock` (`ci.yml:250,285,550,561`). Mismatch fails loudly.
- Tag-to-crate-version agreement gated (`release.yml:106-111`).
- Tag-to-extension-version agreement gated (`release.yml:295-300`).
- Site release fact refreshed from the GitHub releases API each build
  (`site.yml:128-155`).
- RFC count, range, gaps, index membership and cross-references are all gated
  by `compiler/vyrn-cli/tests/rfc_index.rs:131,155,203,242,269`.

### Style sheet

- Palette centralised: oklch literals live only in the three token blocks;
  rules reference tokens exclusively (`site/public/style.css:10-13,24-39,244-292`).
- Font stacks defined once and used through `var()`
  (`site/public/style.css:115-121`).
- Dark-copy drift gated (`site/test/contrast.test.mjs:279`); contrast measured
  for both palettes (`contrast.test.mjs:266`); type scale gated by
  `site/test/typescale.test.mjs`.

## The ten worth fixing first

RECOMMENDATION, NOT A DECISION. Ranked by risk times effort: silent-drift
first, live drift above potential drift, smallest change preferred. Each entry
names the smallest change that would fix it. Fixes belong to separate jobs;
doing them here would conflict.

1. **Keyword tables, including the editor grammar.** Live drift (`logging`,
   `from` miscoloured as keywords). Smallest change: a Rust test asserting the
   tmLanguage keyword alternatives equal the lexer's 24 keywords, then commit
   the regex as generated output. Source: `lexer.rs:31-55`.
2. **LSP builtin-method table.** Live drift: four vector methods have no hover
   or completion. Smallest change: derive `ALL_BUILTIN_METHODS` from
   `parser::METHOD_BUILTINS`, or add a parity test naming the missing four
   today. Sources: `parser.rs:103-140` vs `symbols.rs:3747-3774`.
3. **Stale prose counts.** Already wrong today: "ten consumer pages" (table
   holds 13, `site/app/meta.vyrn:68`), "Five assets per release"
   (`docs/releasing.md:45` vs six at `release.yml:320-346`), "three
   platforms" (`docs/releasing.md:34` vs four legs at `release.yml:72-94`),
   plus the interpolable headings ("Eight programs", "Eleven chapters", "Four
   pages"). Smallest change: interpolate the counters that already exist;
   reword the two releasing.md sentences to name no number.
4. **Contextual word twins.** Ten grammar words held by the playground
   (`compiler/vyrn-play/src/lib.rs:157-159`) and the site highlighter
   (`site/app/hl.vyrn:184-186`) with no comparison. Smallest change: commit a
   generated `contextual` list and have both read it, or one test comparing
   the two literals.
5. **Constructor alias trio.** Six names in three places
   (`loader.rs:381-386`, `symbols.rs:1091`, `symbols.rs:3198`). Smallest
   change: one public constant in `loader.rs`, the other two sites consume it.
6. **Repo slug spellings.** `vyrn-lang/vyrn` typed in four site files plus
   five workflow/doc/script sites. Smallest change: reuse `repoName()`/
   `ghBlob()` on the site side; use `github.repository` in the release
   heredoc. Sources: `site/app/github.vyrn:8-14`; `release.yml:367,371`.
7. **Log-level five.** Five names dispatched in six sites across three crates.
   Smallest change: one ordered table in `ast.rs` plus a macro or a single
   `matches!` fed by it; the ordinal map stays canonical.
8. **Playground limits tooltip.** Three limits copied across JS, worker, Rust
   and a tooltip (`play.vyx:128` et al.). Smallest change: emit a small
   limits file from the wasm build step and `readFile` it in `play.vyx`.
9. **CSS breakpoints.** Six px values across thirteen queries with no tokens
   (`site/public/style.css` rows above). Smallest change: a breakpoints table
   stamped into the sheet by the export generator, since custom properties
   cannot parameterise `@media`.
10. **Action SHA pins.** checkout x9, rust-cache x6, cache x2, upload-artifact
    x3 across three workflows. Smallest change: two composite actions
    (`.github/actions/checkout`, `.github/actions/rust-cache`) wrapping each
    pin once.
