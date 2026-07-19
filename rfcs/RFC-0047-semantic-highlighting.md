# RFC-0047 — Semantic Highlighting, Import Hover, and Grammar Gaps

- **Status:** Draft (design locked)
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

## Out of scope

Semantic tokens inside remote/vendored modules (local + linked only),
inlay hints (type annotations shown inline — a separate feature),
signature help, a full theme, colouring inside string interpolation
holes beyond what the origin map already yields, `.vyx` `<style>` CSS
grammar (no scoped styles yet).
