# RFC-0042 — Template Editor Intelligence: Class, Attribute, and Component Completion

- **Status:** Implemented. See the as-landed notes at the end.
- **Depends on:** RFC-0033 (origin maps — the `.vyx`→generated forward
  mapping this rides), RFC-0032/0036 (`Tw`/`TwClass` finite type + themed
  class attrs with column-exact origins), RFC-0020 M1 (finite-string
  domain completion — the machinery that already exists for `.vyrn`),
  RFC-0039/0041 (the `.vyx` v2 template + layouts this completes)
- **Evidence (user):** "no suggestions/autocomplete for attributes and
  classes (Tw too)." Today the LSP gives hover/completion for `{{ expr }}`
  template expressions (RFC-0033 forward-maps them into the generated
  module), and RFC-0020 built finite-string-domain completion for `.vyrn`
  — but neither reaches a `.vyx` `class="…"` value, an attribute name, or
  a component tag. Tailwind's signature DX (class autocomplete) is absent
  precisely where Vyrn could do it *better*: `TwClass` is a finite type,
  so the completion set IS the compile-checked set.

---

## First: diagnose why class completion doesn't fire (likely a small gap)

The pieces exist. RFC-0036 emits a static `class="…"` as
`vyxTheme.cls("…")` with a **column-exact `//@origin`** at the class
string; RFC-0033 forward-maps a `.vyx` cursor into the governed generated
span; RFC-0020 has finite-domain completion for a string literal that is
an argument to a validated-string-typed parameter. So a cursor inside
`class="flex ga|"` *should* map to the `cls("flex ga|")` argument (a `Tw`
value) and offer the domain. **Step one is to determine exactly why it
doesn't** — probable causes, each a targeted fix, not a rebuild:

1. The forward mapping resolves at expression *boundaries* and doesn't
   place the cursor *inside* a string-literal token → completion never
   sees a string-literal context.
2. Finite-domain string completion is wired for a bare literal in
   argument position but not when reached via the origin map's synthetic
   location.
3. The `Tw` sequence type (`class( class)*`) isn't enumerated for
   completion the way a plain finite type is (it's a regex over a finite
   alphabet, not a finite language) — so completing a *token within a
   space-separated sequence* needs the alphabet (`TwClass`), not the
   whole-string domain.

The report names the actual cause; the design below is what "fixed" means.

## The three completion surfaces (locked behaviour)

### A. `Tw` class values (the headline)

Inside a themed `.vyx` `class="…"` and `:class="…"` string, and inside
`.vyrn` `theme.cls("…")`:

- Completion offers the **`TwClass` alphabet** — every theme-derived
  utility (`bg-brand-500`, `p-4`, `md:hover:…`) plus safelist entries —
  filtered by the token under the cursor (the word between spaces).
  Selecting inserts the class; the surrounding sequence is preserved.
- This is **token-in-sequence** completion: the cursor's whitespace-
  delimited word is the completion prefix, not the whole attribute value.
- The domain is the same `TwClass` the compiler checks against, sourced
  from the theme the `componentsThemed(dir, theme)` / `theme` import
  names — no second enumeration, no drift.
- **Hover** on a class token shows the CSS rule `css()` would emit for it
  (utilities) or "safelisted (app-styled)" (safelist) — the Tailwind-
  plugin experience, from the finite type.
- An unknown token already errors (RFC-0036); completion just makes the
  valid set discoverable before the error.

### B. Attribute names + `v-` directives

At an attribute-name position in a `.vyx` template (the LSP parses the
template enough to know "cursor is naming an attribute on `<tag>`"):

- Offer the standard HTML attribute set (global + a small per-element
  refinement — `href` on `<a>`, `value`/`type` on `<input>`, …), plus
  the Vyrn directive set: `v-if`, `v-else-if`, `v-else`, `v-for`,
  `v-html`, `:` (dynamic-attr prefix), `@` (event prefix), `:key`.
- `@event` completion offers the DOM events the runtime dispatches
  (`click`, `input`, `change`, `submit`, `keydown`, …).
- No validation added — this is discovery; unknown attributes pass
  through as today.

### C. Component tags + props

At a tag position (`<Cap|`) inside a `.vyx`, offer the PascalCase sibling
components the generator would resolve (same-dir `.vyx` files); inside a
component tag, offer its `props {}` names (`:prop=` / `prop=`), typed from
the component's declared prop types (hover shows the type). Unknown
component tag stays the existing generation diagnostic.

## Mechanism (LSP-side; std/vyx surfaces the domains)

- The LSP gains a **`.vyx` template cursor classifier**: given an offset
  in a `.vyx`, decide {template-expression | class-value token |
  attribute-name | event-name | component-tag | component-prop | script}
  — a small template-structure scan reusing `std/vyx`'s tokenization
  rules (shared, not re-invented; expose what's needed as pure helpers or
  mirror the locked grammar).
- For A, the LSP resolves the active theme (the `.vyx` dir's
  `componentsThemed`/`pagesThemed` theme arg, via the loader's generator
  wiring) and asks the frontend for the `TwClass` alphabet — the same
  enumeration `std/tw` builds; surface it as an analysis query
  (`finite_domain_of(type)` already conceptually exists for RFC-0020
  completion — extend/reuse it for the `Tw` alphabet).
- Expression completion inside `{{ }}` (record fields, i18n `t.` methods,
  `TransKey` string domains) is RFC-0033's existing path — verify it
  still fires under RFC-0041 layouts/`head` and extend the finite-string
  fix (issue 1/2 above) so `{{ t("cart.rem|") }}` completes `TransKey`
  too (same class of fix as `Tw`).
- Redeploy `editor/vscode/server/vyrn-lsp.exe` after (frontend/LSP
  change) — and the TextMate grammar already highlights v2; no grammar
  change needed for completion.

## Proof

- In bin + shelf `.vyx`: typing `class="p|` offers `p-4`/`px-2`/… ;
  `md:h|` offers `md:hover:…`; a safelisted `boo|` offers `book-card`;
  hover on `bg-brand-500` shows its CSS. `<a v-i|` offers `v-if`;
  `@cl|` offers `click`. `<Create|` offers `CreateForm`; inside it,
  `:draft|` offers the draft props. `{{ t("cart.|" }}` offers the
  `TransKey` set.
- LSP e2e tests for each surface (class token, attribute name, event,
  component tag, component prop, `TransKey` in `{{ }}`), driven through
  the origin-map forward mapping — the coverage the manual report proves.
- No behaviour change to compilation; this is editor-only. Full suite +
  parity stay green (LSP tests grow).

## Out of scope

Rename/refactor across the mapping (still), signature help, formatting
inside templates, completion for arbitrary-value Tailwind (there is none
— the vocabulary is closed), non-themed `.vyx` class completion (no
domain to offer — plain strings), CSS-language features inside `<style>`
(no scoped styles yet).

---

## As landed

Editor-only work — **zero change to compilation semantics or emitted code**. The
frontend additions are read-only analysis queries; everything else lives in the
excluded `vyrn-lsp` crate. Workspace 903 → **905** (+2 frontend unit tests), LSP
e2e 11 → **17** (+6, one per surface), full three-way parity green, zero warnings.
`editor/vscode/server/vyrn-lsp.exe` rebuilt + redeployed.

### Phase 0 — the diagnosis (the actual why)

Two independent gaps, both from the RFC's candidate list:

1. **The `.vyx` completion path never reached string-literal completion at all**
   (candidate 2). `handle_completion`'s generator-input branch only routed to
   `member_completions` (after `item.`) or top-level `completions` — so a class
   value *and* a `{{ t("…") }}` key never even attempted a finite/sequence domain.
2. **`Tw` is not a finite type** (candidate 3). A static `class="flex gap-2"`
   emits `vyxTheme.cls("flex gap-2")`, whose argument type is `Tw =
   token( token)*` — infinite via the `( token)*` loop, so it is absent from
   `finite_string_types` and enumeration never applied. Token-in-sequence
   completion needs the **alphabet** (`TwClass`), not the whole-string domain.

A third, smaller gap surfaced while wiring it: the checked bridge is named `cls`
in both `std/html` (plain `cls(String)`) and `std/tw` (`cls(Tw)`), and a **linked**
module lowers the `Tw` alias to its `String` base in the parameter type — so
matching the expected type by the callee's declared parameter is unreliable.
The fix gates on the `cls(…)` **call context** (the stable public bridge name),
not the lowered parameter type.

### The alphabet, from the same DFA the compiler checks against

`Dfa::enumerate_without(exclude, cap)` enumerates every accepted string that
contains no separator byte; `finite::enumerate_alphabet(decl, cap)` applies it to
an *infinite* pure-regex string type, yielding its single-token alphabet
(`L(Tw)`'s space-free members `= L(TwClass)`). One enumeration, no drift — the
alphabet is derived straight from `Tw`, not re-listed. `Analysis` gained
`sequence_string_types` (name → alphabet, cap 8192) alongside the existing
`finite_string_types`, and `tw_css` (the `css()` baked constant, captured only
when a sequence type is present) for hover.

### Phase A — `Tw` class completion (wired + new)

`class_completions` / `class_token_hover` (frontend queries) gate on the `cls(…)`
argument context and offer the first sequence type's alphabet / CSS. Reached two
ways: in a themed `.vyx`, the RFC-0033 origin map forward-maps a `class="…"`
cursor into the generated `vyxTheme.cls("…")` string (verbatim region, column
exact — RFC-0036 already emits the per-class `//@origin`); in `.vyrn`,
`theme.cls("…")` routes through the existing string-literal branch. The LSP
computes the whitespace-delimited token in the *buffer* line and returns each item
as a `textEdit` replacing only that token, so `md:hover:bg-…` never duplicates the
typed prefix. Hover extracts the exact rule from `css()` (`css_rule_for` handles
base *and* escaped variant selectors, with a prefix-collision guard so `.p-2` is
not satisfied by `.p-20`) or reports "safelisted (app-styled)".

### Phase B — attributes / directives / events (new)

A `.vyx` cursor classifier (`templates.rs`) scans the raw template — tracking
`<script>`, `{{ }}`, and quote-aware tag boundaries (so a `>` inside
`v-if="a > b"` doesn't close the tag) — and classifies the cursor as
`TagName | AttrName | EventName | ClassValue | Other`. At an attribute-name
position it offers global + per-element HTML attributes and the `v-*` directive
set; after `@`, the DOM event vocabulary. Discovery only — nothing new is
validated.

### Phase C — component tags + props (new)

At `<Cap…` the classifier offers sibling PascalCase `.vyx` basenames
(`sibling_components`); inside a component tag, an attribute position offers that
component's declared props (name + type, plus the `:prop` dynamic form) parsed
from the sibling's `props { … }` block (`component_props`).

### Phase D — expression completion under the new surfaces (wired)

The single `.vyx` string-literal routing fix (Phase 0 gap 1) also makes
`{{ t("cart.rem|") }}` complete the `TransKey` finite domain: the forward map
lands in the generated `t("…")` string, `class_completions` declines (not a `cls`
call), and `string_literal_completions` supplies the keys — the same fix class as
`Tw`, verified for a `.vyx` template.

### Verification

Six LSP e2e tests (class-token incl. safelist + `md:hover:` variant, class hover
CSS/safelisted, TransKey-in-`{{}}`, component tag, component prop, attribute +
event), each driven through a scratch fixture and the origin-map forward mapping;
two frontend unit tests (alphabet enumeration + `css_rule_for`). Manual smoke
against the real `examples/bin`, `examples/shelf`, and `examples/twdemo`: a
themed `.vyx` `class="iss|"` offers `issues`; hover on `bg-brand-500` shows
`.bg-brand-500 {background-color:#4f46e5}` and on `md:hover:bg-brand-600` the
escaped `:hover` variant rule; `.vyrn` `theme.cls("…bg-b|")` offers
`bg-brand-500`/`bg-brand-600`.
