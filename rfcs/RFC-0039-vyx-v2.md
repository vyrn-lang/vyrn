# RFC-0039 — `.vyx` v2: Vue-Flavored Templates, `.vyx` Pages, Real Parsing

- **Status:** Draft (design locked)
- **Depends on:** RFC-0026 M4 (the `.vyx` compiler this revises), RFC-0033
  (origin maps — fidelity improves here), RFC-0036 (`componentsThemed` —
  class checking carries over), RFC-0031 (`TypeInfo.module` — import
  handling), the bin + shelf dogfoods (the evidence)
- **Evidence (user review of examples/bin):** `view.vyrn` is hyperscript
  soup — the compile target leaked back into being the surface because
  `.vyx` pages never shipped and Int64-only route segments pushed `/p/<id>`
  into hand-dispatch; the template grammar is Svelte-flavored where the
  user (a Nuxt/Vue developer) expected Vue; and `CreateForm.vyx` carries
  two named crutches — a comment warning that the word `props` may not
  appear in comments (the byte-scan parser matches keywords inside
  comments), and nine threaded scalar props existing only because
  component scripts cannot share imports without duplication.

---

## 1. The template grammar becomes Vue-flavored (breaking, migrate all)

One grammar, not two — the Svelte-flavored forms are REMOVED and every
`.vyx` in the repo migrates (pre-1.0; the corpus is small and mechanical).

| construct | v2 (Vue-flavored) | replaces |
|---|---|---|
| interpolation | `{{ expr }}` (escaped) | `{expr}` |
| raw HTML | `v-html="expr"` on an element | `{@raw expr}` |
| conditionals | `v-if="c"` / `v-else-if="c"` / `v-else` as attributes on elements/components | `{#if}…{:else if}…{:else}…{/if}` |
| loops | `v-for="x in expr"` + **required** `:key="expr"` on the repeated element | `{#for x in e key={…}}…{/for}` |
| dynamic attr | `:name="expr"` (incl. `:class`, `:value`) | `name={expr}` |
| static attr | `name="…"` (unchanged; `class` stays compile-checked when themed) | unchanged |
| events | `@click="handler"` / `@click="handler(arg)"` (unchanged — already Vue-shaped) | unchanged |
| children | `<slot/>` | `{children}` |
| component props | `<CreateForm :heading="expr" title="static"/>` | `prop={expr}` |

- Semantics are UNCHANGED — this is surface syntax over the same lowering
  (conditionals still nest to `Empty`-elided chains, `v-for` still lowers
  to `map` + `keyed`, `:class` is still a runtime `Tw` coercion while
  static `class` is consteval-checked, events still dispatch by name with
  one scalar arg). Sibling-chain rule: `v-else-if`/`v-else` must be the
  immediately following element sibling (whitespace/comments between are
  fine); anything else is a named diagnostic.
- `v-if` grouping without a wrapper element (`<template v-if>`) is
  deferred — noted, not built.
- **Diagnostics keep their classes** (missing `:key`, non-scalar event
  arg, unknown component, …) with wording updated to the new spellings.
- **Origin-map fidelity improves as part of this**: `{{ expr }}`,
  `v-if`/`v-else-if` conditions, `v-for` heads, `:attr` values, and
  `@event` args each get their own column-exact `//@origin` (M4 left
  events/dynamic attrs region-level — that gap closes here).

## 2. The section parser becomes a real scanner

The `<script>`/`<template>`/`props` recognition is rewritten as a proper
scanner that understands line comments, block strings, and attribute
quoting — keyword matches inside comments or strings are impossible by
construction. The `CreateForm.vyx` warning comment is deleted as the
regression test: a `.vyx` whose comments mention `props`, `template`,
and `script` compiles identically to one without them.

## 3. Import dedup in the synthesized module (kills scalar-threading)

`components`/`componentsThemed` merge the import lines of all component
scripts: identical `import { X } from "spec"` lines collapse; same-spec
selective imports union their name sets; `import * as ns` dedups by
(ns, spec); a CONFLICT (same name from different specs across two
components) is a generation diagnostic naming both files. Consequence:
a component script imports `i18n("../strings")` or wire types directly,
and bin's nine-prop `CreateForm` drops to its two honest props (draft
state — the root owns state; that threading stays by design).

## 4. `.vyx` pages (the M4 deferral, closed)

`pages(dir)` (and a themed variant mirroring RFC-0036) accepts
`<name>.vyx` page files alongside `.vyrn` ones:

- A page `.vyx`'s `<script>` may declare `params { id: String, … }`
  (bracket-segment binding, replacing M3's `type Params` record for
  `.vyx` pages), plain helper fns, imports — and optionally
  `fn load(<params>) -> Validation<Data>`; the template is the page body
  (the router still wraps it in `document()` via the app shell
  convention).
- Mechanism: `std/ui`'s gen fn does its own `readFile` (comptime
  builtins stay direct in the gen-fn body) and calls `std/vyx`'s
  **exposed pure compiler core** (plain exported fns — VERIFY std↔std
  pure-fn imports early; if the loader fights it, that finding is a
  primary deliverable, not something to fork the compiler source over).
- The M4-era blocker (flat positional props vs Params record calling
  convention) dissolves: the pages generator owns the synthesized call
  site on both sides, so it binds segments/loader data to the compiled
  template fn directly.

## 5. `std/ui` route segments: `String` joins `Int64`

`[id].vyx` / `[id].vyrn` segments may be `String` (declared in
`params {}` / `Params`); a `String` segment matches any non-empty,
non-`/` segment (decoded), `Int64` keeps its integer rule. This deletes
bin's hand-dispatched `/p/<id>` and `/raw/<id>`. Additionally the pages
surface gains a **raw response** escape: a page module/`.vyx` may export
`fn respond(p: Params) -> Response` INSTEAD of `page` (full control —
content-type, status), which is how `/raw/[id]` lives inside the router
instead of beside it.

## Migration & proof

- Migrate every `.vyx` (shelf, bin, vyxcomp/vyxdomcomp examples) and the
  emit-gen goldens; `examples/bin/view.vyrn` dissolves into `.vyx`
  components + `.vyx` pages with only thin glue left in the root; delete
  the props-comment crutch and the nine-prop threading.
- Browser-verify both apps fully (bin: create/restart-survival/raw/404;
  shelf: the standard pass). CLI + LSP: origin-mapped diagnostics at the
  new column-exact sites; `vyrn-lsp.exe` redeployed.
- Zero compiler changes expected (generator + std/ui work; the scanner
  and grammar live in `std/vyx`). Any compiler-adjacent need = report.

## Out of scope

`v-model` (two-way sugar — the explicit value/@input pair stays honest
about who owns state), `<template>` grouping, scoped styles, `v-show`,
component-local state (unchanged position), the clock/random and
std/storage findings (separate upcoming designs).
