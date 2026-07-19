# RFC-0048 — Complete `.vyx` Origins: Script Sections & Real-File Pages

- **Status:** Draft (design locked)
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
