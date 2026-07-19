# RFC-0048 — Complete `.vyx` Origins: Script Sections & Real-File Pages

- **Status:** Implemented (§1–§3 as-landed 2026-07-19; see "As landed")
- **Status (historical):** Draft (design locked)
- **Depends on:** RFC-0033 (origin maps — the directive contract this
  completes), RFC-0047 (semantic tokens + import hover — the LSP machinery
  already built to CONSUME these origins), RFC-0039/0041 (the `.vyx`
  component/page/layout compilers being taught to emit them)
- **Evidence (RFC-0047 walls, user review):** RFC-0047 shipped the LSP
  side — semantic tokens, import-specifier classification, import hover —
  but it lights up nothing in the two places the user actually looked:
  (1) `.vyx` `<script>` sections emit **no** `//@origin`, so import
  specifiers there (`tBinCount`, `Locale`, `format`) have nothing to map
  through — no hover, no colour; (2) `pages`/`layout`/`error` `.vyx`
  compile through a **synthetic `UiPageBody.vyx`** with synth-relative
  lines, so `routes/index.vyx` / `layout.vyx` get **zero** LSP
  intelligence (no tokens, no class completion). Both are generator-layer;
  the LSP already handles the origins once emitted.

---

## 1. Emit `//@origin` for the `.vyx` `<script>` section

`std/vyx` copies a component/page/layout `<script>` largely verbatim into
the synthesized module (imports pass through, helper `fn`s pass through,
`props`/`params` become a signature). Today it emits origin directives
only for template expressions. Extend it to emit **column-exact
`//@origin <file>:<line>:<col>`** for the verbatim-copied script regions:

- **Import lines** (`import { X } from …`, `import * as ns from …`) — each
  copied line gets an origin at its real `.vyx` position, so a specifier
  (`X`) and a namespace (`ns`) resolve/hover/classify (RFC-0047's
  classifier and hover already map verbatim origin regions — no LSP
  change).
- **Helper `fn` bodies** copied verbatim — origin at their real position.
- **`props`/`params` field types** — origin at the field, so hover/colour
  reach the declared types.
- Regions the generator *synthesizes* (the wrapper signature, the
  `vyxTheme` import it injects) get no origin (region-level, as today) —
  they aren't in the user's buffer.
- Directives are inert comments (RFC-0033 guarantee): **runtime output is
  byte-identical**; only `emit-gen` text and the LSP change. emit-gen
  goldens are regenerated deliberately and reviewed.

## 2. Pages/layouts/errors compile with **real-file** origins

`std/ui`'s `vyxBuildPageModule` compiles each page via
`vyxCompileComponent("UiPageBody", synthSource, dir)` — a synthetic file
name and synth-relative line numbers, so every origin points at a file no
editor buffer corresponds to. Fix the wiring so origins point at the
**real route file**:

- The page/layout/error compile threads the **real input path**
  (`routes/index.vyx`, `routes/layout.vyx`, `routes/error.vyx`) and the
  **real line/column offsets** of the template and script regions within
  it, so `//@origin` targets the real file at real coordinates.
- The synthesized wrapper the generator wraps around the page body (the
  `page(...)`/`respond(...)`/`layout(children)` shell, the params binding)
  stays origin-less — it isn't user text. The user's template + script
  map through; the glue doesn't.
- Consequence: the LSP's `vyx_owner` registry (input file → synthesized
  module) now resolves `routes/index.vyx` to a real generated module, so
  RFC-0042 class completion + Tw hover and RFC-0047 semantic tokens +
  import hover all fire in pages and layouts — the same machinery, now
  fed real origins.
- **Page rendering is unchanged** — the compiled `page`/`respond`/`layout`
  function is byte-identical; only the origin directives and internal line
  mapping change. Prove it: the bin/shelf/fullstack page emit-gen output
  differs ONLY in `//@origin` lines and the serve/browser behaviour is
  identical.

## 3. Consumers / proof (the user's exact positions light up)

After this, in `examples/bin`:

- **`routes/layout.vyx`** `<script>`: hover `format`/`fromMillis` →
  their `std/time` signatures; they colour as **function**, not variable.
  `import * as t` → `t` colours as **namespace**, `as` already a keyword.
- **`routes/index.vyx`** `<script>`: hover `listPastes` → its signature;
  the `i18n` import specifiers hover with their translations.
- **`routes/index.vyx`/`layout.vyx`** template: `class="mr-2
  hover:text-brand-600"` offers **Tw class completion** and hover shows
  the CSS (RFC-0042, now reachable in pages); `head { module "/app.js" }`
  keywords colour (RFC-0047 grammar, already shipped — confirm).
- **`.vyx` script** identifiers generally colour by kind (functions,
  types, params) via the RFC-0047 classifier now that the script region
  has origins.

Reported as a live LSP transcript at those exact positions (hover text +
semantic-token kinds), the way RFC-0047 reported the `.vyrn` side.

## Verification

- emit-gen goldens regenerated; the diff is **only** `//@origin` lines
  (no semantic change to generated code) — state this explicitly per
  golden.
- Full workspace suite + LSP e2e (grow: `.vyx` script-section import
  hover + classification; page class completion + semantic tokens on a
  real route file). **Full three-way parity green** — directives are
  comments, runtime output unchanged (the load-bearing invariant: prove
  bin/shelf still serve byte-identically).
- Rebuild is generator-side (`std/vyx`/`std/ui`); the LSP binary is
  unchanged, but re-confirm the deployed `vyrn-lsp.exe` is HASH-current
  with HEAD anyway (the stale-binary discipline).
- Browser: bin/shelf pages still render + hydrate identically.

## Out of scope

Origins inside the generator-synthesized glue (wrapper signatures, the
injected theme import — no user text there), remote/vendored `.vyx`,
`.vyx` `<style>` blocks (none yet), inlay hints, and any change to what
the generators *emit as runtime code* (this RFC only adds comment
directives + fixes line mapping — zero runtime delta).

---

## As landed (2026-07-19)

Both sections shipped entirely in the generator layer (`std/vyx.vyrn`); the
LSP binary and `std/ui.vyrn` are **unchanged** (`vyrn-lsp.exe` hash-verified
equal to a fresh release build of HEAD). The load-bearing invariant held: for
every `.vyx`-using example (`bin`, `shelf`, `fullstack`), the `emit-gen`
output with `//@origin` lines stripped is **byte-identical** before and after —
the only diff is added `//@origin` comment lines. Full three-way parity green,
926 workspace + 22 LSP tests (2 added), 0 warnings.

**§1 — script-section origins (`std/vyx`).** `vyxParseScript` became a
position-aware core, `vyxParseScriptAt(ba, sStart, sEnd, …)`, walking the
`<script>` section over WHOLE-FILE offsets and recording, per copied line, its
1-based `.vyx` `(line, col)` (first-non-ws anchor):

- **Imports** — `VyxComp` carries `importLines`/`importCols` parallel to
  `imports`; `vyxMergeImports` emits each merged import bracketed by
  `//@origin <file>:<line>:<col>` … `//@origin end`. A single-source bucket
  (the norm, and every page/layout) is verbatim → column-exact; a bucket that
  UNIONED two components' same-spec imports is derived → emitted origin-less
  (graceful, per RFC-0033). The import CODE is untouched — only the surrounding
  comment lines are added — so `import { format, fromMillis } from "std/time"`
  now hovers/classifies each specifier, and `import * as t` classifies `t` as a
  namespace.
- **Helpers** — `helperTexts`/`helperLines`/`helperCols` carry each pass-through
  helper line; `vyxHelperBlock` brackets each with its own directive, so a
  helper `fn`/`type` and its body identifiers map back (each line is its own
  region — the LSP maps a region's first generated line, which for verbatim
  one-line copies is the line itself).
- **Prop/param field types** were left origin-less (they are consumed into the
  synthesized fn signature / synthetic `props` block — generator text — and the
  money-shot identifiers are all imports/helpers). A future extension.

**§2 — real-file page/layout/error origins (`std/vyx`).** The page/layout/error
builders gained `*At` variants threading the REAL route-file path
(`vyxBuildPageModuleAt` / `vyxBuildLayoutModuleAt` / `vyxBuildErrorModuleAt`);
the `vyxPage`/`vyxLayout`/`vyxError` gen fns pass the `.vyx` path they already
receive. The compile still goes through the synthetic source (page rendering
code is byte-identical), but the result is **relocated** onto the real file by
`vyxRelocateComp`:

- **Template** — the synthetic `<template>` body is a verbatim, newline-aligned
  copy of the real template at a shifted line, so a single constant
  `dLine = realTemplateStartLine − synthTemplateStartLine` (columns identical)
  plus `srcPath = <real file>` makes every template origin land on the real
  `.vyx` at real coordinates. The AST is line-shifted by `vyxShiftNode`.
- **Script** — import/helper origins are re-derived from the REAL file
  (`vyxParseScriptAt` on the real bytes) and matched onto the emitted comp by
  identity (imports) / content+order (helpers, ignoring a re-added `export `);
  the error page's injected `PageError` import and any unmatched line stay
  origin-less. So `routes/index.vyx` / `layout.vyx` now emit origins like
  `//@origin ./routes/index.vyx:7:1`, and the LSP's `vyx_owner` registry —
  which auto-derives from `origins.input_files()` — resolves the REAL route file
  to the generated router module with **no LSP change**. RFC-0042 class
  completion + Tw hover and RFC-0047 semantic tokens + import hover now fire in
  pages and layouts (they returned nothing before, when origins pointed at the
  synthetic `UiPageBody.vyx`).

**§3 — proof.** New LSP e2e tests
`rfc48_vyx_script_import_hover_and_classification` (script import hover +
`format`/`fromMillis`→function, `clk`→namespace, helper `shown`→function) and
`rfc48_page_semantic_tokens_and_class_completion` (a real `routes/index.vyx`:
non-empty semantic tokens + `format`→function + `p-4` Tw completion). A live
transcript over `examples/bin` confirms the user's exact positions light up:
`listPastes`/`format`/`fromMillis`/`i18n` hover with signatures, `t`→namespace,
`class="mr-2 hover:text-brand-600"` offers Tw completion + CSS hover, and
`routes/index.vyx` semantic tokens went from 0 to non-empty.

**RFC-0047 walls resolved:** §2 (`.vyx` import hover blocked in the generator)
and §4 (pages not LSP-wired) are both closed here — see RFC-0047 "As landed".
