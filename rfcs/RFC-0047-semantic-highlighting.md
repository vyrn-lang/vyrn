# RFC-0047 — Semantic Highlighting, Import Hover, and Grammar Gaps

- **Status:** Implemented (§1–§3 as-landed; §4 diagnosed, blocked in the
  generator layer — see "As landed")
- **Status (historical):** Draft (design locked)
- **Depends on:** RFC-0033 (origin maps — mapping tokens into `.vyx`),
  RFC-0027 (namespace hover — the hover path extended here), RFC-0042
  (template completion — the same `.vyx` cursor/analysis machinery),
  RFC-0041 (`.vyx` `head` block — a grammar gap)
- **Evidence (user review):** imported functions (`format`, `fromMillis`)
  are colored as blue variables; `as`, and the `.vyx` `head`/`module`
  block keywords, aren't highlighted; there are no tooltips on imported
  names (`tBinCount`, `Locale`, …). **Diagnosed:** the LSP emits **no
  semantic tokens** — all coloring is the two purely-syntactic TextMate
  grammars, which cannot distinguish function/type/variable/parameter
  (that needs name resolution). This is the same "functions and variables
  share a color" complaint from early in the session, now root-caused.

---

## 1. LSP semantic tokens (the root fix)

`vyrn-lsp` implements `textDocument/semanticTokens/full` (+ `/range`),
classifying every identifier from the already-computed `Analysis` so the
editor overlays precise colour on top of TextMate:

- **Token types (LSP standard set):** `function` (calls + defs + imported
  fns), `type`/`struct`/`enum` (type names, incl. imported), `enumMember`
  (variant names), `parameter`, `variable` (`let`/loop binds), `property`
  (record fields, `.field`), `namespace` (`import * as ns`, and `ns.` uses),
  `keyword` (contextual keywords TextMate misses, §3), `method` (protocol/
  builtin methods), plus `macro` for the compiler builtins
  (`toJson`/`slice`/`bytes`/…) so they read distinctly from user calls.
- **Modifiers:** `declaration` (the defining occurrence), `readonly`
  (non-`mut` binds, `Instant`-style validated types), `defaultLibrary`
  (std/builtins).
- **Import specifiers get their real kind:** in `import { format, Locale }
  from …`, `format` is `function` and `Locale` is `type` — the headline
  fix. The classifier resolves each specifier against the imported
  module's exports (the loader already links them; generator imports —
  `i18n(...)`, `rpcClient(...)` — resolve through the synthesized module's
  exports exactly as go-to-def already does).
- **`.vyx` coverage:** the script section is classified directly; template
  `{{ expr }}` / `:attr` / `@event` expressions are classified by mapping
  their tokens back through the origin map (RFC-0033) into the generated
  module's analysis — so a `{{ t.appTitle() }}` colours `t` as namespace
  and `appTitle` as function inside the template. Region-level spans that
  don't map stay TextMate-only (no regression).
- Registered in the server capabilities with the token legend; VS Code
  needs no setting beyond the theme (semantic highlighting is on by
  default). Performance: computed from the cached `Analysis`, no reparse.

## 2. Hover on imported names (and everywhere the token resolves)

Hovering any resolved identifier shows its signature + type + `///` doc —
extended to the positions the user found bare:

- **Import specifiers:** hover `tBinCount` → its generated signature +
  the source-locale message (i18n already puts the translation in the
  `///` doc); `Locale` → the enum + its variants; `format` → `fn
  format(Instant) -> String` + std doc. Covers std, user, and generator
  imports.
- Consistency: the same hover fires on the *use* site and the *import*
  site (today it's patchy) — one resolver, both positions.

## 3. TextMate grammar gaps (the syntactic complement)

Semantic tokens need the LSP running; the grammar is the fallback and
handles pure keywords. Fill the gaps the user hit and audit the rest:

- **`vyrn.tmLanguage.json`:** add `as` (import aliasing), and audit for
  any missing keyword — `gen`, `extern`, `spawn`, `region`, `protocol`,
  `impl`, `where`, `drop`, `consume`, `test`, `match`/`else if` — colour
  any that are bare.
- **`vyx.tmLanguage.json`:** add the block keywords `head`, `module`,
  `script`, `stylesheet`, `title` (RFC-0041 head block), and confirm
  `slot`, `layout`, `params`, `props`, `v-if`/`v-else-if`/`v-else`/
  `v-for`/`v-html`, `:attr`, `@event` are all coloured.
- These are cheap and load instantly (no server), so they also fix the
  first-paint-before-LSP flash.

## 4. Verify RFC-0042 completion reaches pages (post-stale-binary)

The deployed server was found stale (a redeploy that didn't match HEAD).
With the corrected binary: confirm `.vyx` **class completion + hover**
(RFC-0042) fires in **`pagesThemed` pages** (e.g. `routes/index.vyx`), not
only `componentsThemed` components — the theme must resolve from a page's
generator wiring too. If a page's themed-class context isn't recognized,
fix that resolution (same mechanism, different generator entry).

## Deliverables / proof

- Semantic tokens live: a scripted LSP `semanticTokens/full` over
  `bin/routes/index.vyx` + `bin/client.vyrn` returns `format`→function,
  `Locale`→type, `t`→namespace, a `let` bind→variable, `Paste`→type,
  a field→property — reported as the proof transcript.
- Hover transcript at the import-specifier positions the user named.
- Grammar: `as`/`head`/`module` coloured (a screenshot-equivalent: the
  scope names the tokens now carry).
- **Rebuild + redeploy `vyrn-lsp.exe`, HASH-VERIFIED against a clean
  build of HEAD** (the stale-binary discipline — mtime is not proof).
- Full suite + LSP tests green (semantic-token unit tests added); 0
  warnings; no compiler-semantics change (LSP + grammar only).

## As landed (2026-07-19)

**§1 — LSP semantic tokens (shipped).** `vyrn-lsp` now serves
`textDocument/semanticTokens/full` + `/range`. The classifier
(`vyrn_frontend::semantic_tokens` / `classify_at`, a read-only pass over the
cached `Analysis`) mirrors `resolve`'s precedence exactly — local → `ns.member`
→ record-field member → namespace binding → top-level symbol → builtin — so a
colour always agrees with hover.

- **Token legend (order = wire indices):** `namespace`, `type`, `enumMember`,
  `parameter`, `variable`, `property`, `function`, `method`, `macro`, `keyword`.
  Modifiers: `declaration` (bit 0), `readonly` (bit 1), `defaultLibrary` (bit 2).
  `keyword` is registered but not emitted (the grammar owns keywords, §3).
- **Import specifiers get their real kind** — the headline. In
  `import { X, Y } from …`, each specifier resolves against the imported
  module's exports (indexed by `index_imported_symbols`, exactly as go-to-def).
  Proven on the real `examples/bin/client.vyrn`: `Html`→type,
  `toHtmlString`/`diff`/`createForm`→function, `rpcClient`→function — including
  **generator** imports (`createForm` resolves through the `componentsThemed`
  synthesized module's exports, `api`→namespace through `rpcClient(...)`).
  `fromJson`/`toJson`→macro; module state `draftTitle`→variable(declaration);
  `RawCreate`→type(declaration); params→parameter; record fields→property.
- **`.vyx` coverage — template only.** `.vyx` tokens are classified by mapping
  each verbatim RFC-0033 origin region back from the synthesized module's
  classification into input coordinates (`vyx_semantic_tokens`; the synth module
  is analyzed once per generated banner). Verified on the wired component
  `examples/bin/widgets/CreateForm.vyx`: `{{ tBinCreate() }}`→function,
  `v-if="issues…"`→parameter, `:value="draftTitle"`→parameter. **The `<script>`
  section is NOT covered** — the `.vyx` generators (`std/vyx`) emit `//@origin`
  directives only for template expressions, never for the script region, so
  script-section identifiers (crucially, **import specifiers**) have no origin to
  map through. This is a generator gap, not an LSP gap (see §2/§4 below).

**§2 — Import hover (shipped for `.vyrn`; `.vyx` blocked in the generator).**
The hover handler was already resolving import specifiers at both the import
site and the use site for `.vyrn` (one resolver — `resolve` matches the imported
symbol by name); RFC-0047 adds an e2e test locking it. Verified on
`client.vyrn`: hover `rpcClient`→`fn rpcClient(contract: String) -> String`,
`createForm`→its full generated signature, `Html`→the enum + variants,
`diff`→its signature. **The user's exact positions** (`tBinCount`/`setLocale`/
`locale`/`Locale` and `format`/`fromMillis` in a `.vyx` *import list*) do **not**
resolve — those specifiers live in the `.vyx` `<script>` section, which has no
origin map (above). Fixing it needs `std/vyx` to emit `//@origin` for the script
region — an emitted-code (generator) change, out of this task's LSP/grammar/
read-only-frontend scope and under the parity invariant. **Recorded as a wall.**
**RESOLVED by RFC-0048 §1 (2026-07-19):** `std/vyx` now emits `//@origin` for
the script region's import + helper lines, so those specifiers (`format`,
`fromMillis`, `i18n`, `import * as t`, `listPastes`) hover and classify. No LSP
change was needed — the existing verbatim-region forward map consumes them.

**§3 — Grammar gaps (shipped).**
- `vyrn.tmLanguage.json`: added a `#contextual-keywords` block colouring `as`
  (`keyword.control.vyrn`, matched only before an identifier — `import { x as y }`
  / `* as ns`), and `gen` / `extern` (matched only before `fn`), so bindings
  named `as`/`gen`/`extern` stay `variable.other`. `consume` was already covered
  by `#capabilities`; every other audited keyword (`region`/`spawn`/`protocol`/
  `impl`/`where`/`drop`/`test`) was already present.
- `vyx.tmLanguage.json`: added the RFC-0041 head-block keywords in the `<script>`
  section — `head` (before `{`) → `keyword.control.head.vyx`, and
  `module`/`stylesheet` (before a string) / `title` (before `:`) likewise,
  matched contextually. Confirmed `props`/`params`, `v-if`/`v-else-if`/`v-else`/
  `v-for`/`v-html`, `:attr`, `@event`, and `<slot/>` (an element tag) are coloured.

**§4 — Pages are not LSP-wired (diagnosed; blocked in the generator).** With the
hash-verified binary, class completion + hover fire in `componentsThemed`
components (verified: class completion in `CreateForm.vyx` at `class="create"`
offers `create`/`count`/…). But **`pagesThemed` pages get zero LSP intelligence**
(`routes/index.vyx` → 0 semantic tokens, empty class completion). Root cause:
`std/vyx`'s `vyxBuildPageModule` reconstructs a synthetic component source and
compiles it via `vyxCompileComponent("UiPageBody", synthSource, dir)`, so every
page's origin directives point at a synthetic **`UiPageBody.vyx`** (with
`synthSource`-relative line numbers), never the real route file. The LSP wires
`UiPageBody.vyx` / `UiLayoutBody.vyx` / `UiErrorBody.vyx` (no editor buffer
corresponds) instead of `index.vyx` / `about.vyx`. This is not a theme-resolution
issue — the page's `.vyx` is never mapped at all. The fix is a `std/ui` +
`std/vyx` generator change (compile pages with real-file origins + true line
numbers, and emit script-section origins), which alters emitted generator code
and carries `std/vyx` unit-test + three-way-parity risk — outside this task's
LSP/grammar/read-only-frontend scope. **Recorded as a wall with options:**
(a) thread the real source path + a `synthSource`→source line map through
`vyxBuildPageModule`/`vyxCompileComponent`; (b) additionally emit `//@origin`
for the script region so import specifiers hover; (c) both, as a dedicated
follow-up RFC, verified under the parity harness.
**RESOLVED by RFC-0048 §2 (2026-07-19):** option (c) landed — the page/layout/
error builders thread the real route-file path and relocate the compiled comp
(template line-shift + real script origins) onto it, so `routes/index.vyx` /
`layout.vyx` now get semantic tokens + class completion + import hover. The
`vyx_owner` registry auto-wires from the new origins with no LSP change; the
page rendering code is byte-identical (proven via `emit-gen`, parity green).

## Out of scope

Semantic tokens inside remote/vendored modules (local + linked only),
inlay hints (type annotations shown inline — a separate feature),
signature help, a full theme, colouring inside string interpolation
holes beyond what the origin map already yields, `.vyx` `<style>` CSS
grammar (no scoped styles yet).
