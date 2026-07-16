# RFC-0026 — The UI Layer: `std/html`, Pages, Components, and Compiled Reactivity

- **Status:** Draft (design locked, implementation staged M1→M4; M5+ deferred)
- **Depends on:** RFC-0012 (extern, both directions), RFC-0013 (module state +
  host-owns-the-loop), RFC-0016 (`serve`/`handle`), RFC-0018/0024 (the codec —
  the view tree crosses the boundary as a codable enum), RFC-0019 (`std/rpc` —
  the library-not-keyword precedent this RFC must match), RFC-0020 (finite /
  regex-validated string types — typed URLs, checked classes), RFC-0021
  (generator imports — the ONLY compilation mechanism this RFC is allowed to
  use), RFC-0023 (lambdas — `map` in view code)
- **Evidence:** the fullstack demo builds HTML by string concatenation; the
  browser counter (RFC-0013) hand-wires DOM events; the stated project goal is
  a Nuxt-class stack; and the DX critique of raw hyperscript + string-named
  handlers is correct and on the record.

> **The prime constraint (the acceptance criterion).** This RFC ships **zero
> compiler, checker, or codegen changes**. Every layer is a library (`std/html`,
> `std/ui`) or a host runtime file (`web/vyrn-dom.js`) built on public,
> already-shipped primitives: payload enums + codec, extern, module state,
> generator imports with `readFile`/`listDir`/`moduleInterface`, validated
> string types. If implementation hits a language wall, the wall gets its own
> RFC (the RFC-0023 precedent) — it is NOT patched around inside this one.
> Consequence: any third party can build a competing framework with the same
> primitives; nothing here is privileged.

---

## The architecture (why it must be this shape)

Vyrn has no stored closures (RFC-0023 refused that bill), no runtime
reactivity graph, and a host that owns the loop (RFC-0013). Deriving a UI
model from those constraints lands on The Elm Architecture with host-side
diffing:

```
  store (module state, client root)
      │  view() — pure functions returning an Html tree
      ▼
  Html (a codable payload enum)
      │  toJson across the extern boundary
      ▼
  web/vyrn-dom.js — builds/diffs/patches real DOM, wires events by NAME
      │  DOM event → exported extern handler (String arg)
      ▼
  handler mutates the store, host re-renders           (and around again)
```

- **Components are pure functions** of their inputs. No component-local
  state in v1 — state lives in the client root, named and visible (`state {}`
  sugar is deliberately deferred; it would invent a third storage class).
- **Events dispatch by name** to root-exported handlers. This is forced, not
  chosen: module state is root-only (RFC-0013), so only the root can mutate
  the store; and there are no closures to attach. M4's template syntax
  compiles down to exactly this — the strings exist only in generated code.
- **SSR and the client share `view()` verbatim** — the server renders
  `toHtmlString(view())`, the client ships the same tree as JSON. Both are
  pure, so components are parity citizens and snapshot-testable in `vyrn test`.
- **Reactivity is a compile-time concern** (M5, deferred): the template
  compiler statically knows which bindings read which store fields, so the
  escalation path is Svelte-style compiled patches — not runtime signals.

---

## M1 — `std/html`: the tree and the string renderer

Plain Vyrn, pure, a parity citizen.

```vyrn
type Attr =
    | Cls(String)            // class="…" (M4 checks it against Tw when present)
    | Id(String)
    | A(String, String)      // any attribute: A("href", "/items")
    | On(String, String, String)   // event, handler name, payload
    | Key(String)            // list identity for the M2 differ

type Html =
    | Empty                  // renders as nothing; the unit of conditionals
    | Text(String)           // ALWAYS escaped
    | Raw(String)            // NOT escaped — the loud, greppable escape hatch
    | El(String, Array<Attr>, Array<Html>)
```

`Html` is a self-recursive named payload enum (recursion through
`Array<Html>` in a payload). RFC-0024 built recursion-safe per-type codecs
for exactly this shape; **verifying the checker accepts the self-reference is
implementation step zero** — if it doesn't, that fix is a general codec/checker
gap, in scope for a separate commit, not a UI special case.

Helpers (all trivial constructors so view code reads as structure):
`el(tag, attrs, kids)`, `text(s)`, `cls(s)`, `attr(n, v)`,
`on(event, handler, payload)`, `keyed(k, node)`, `empty()`.

`toHtmlString(h: Html) -> String` — the SSR renderer:

- **Escaping (locked):** `Text` escapes `& < > "`; attribute values escape
  `& "`. `Raw` bypasses — it is the only way to emit markup from a string.
- **Void elements (locked):** the HTML spec set (`area base br col embed hr
  img input link meta source track wbr`) renders without a closing tag;
  children passed to a void element are ignored (documented — `toHtmlString`
  is total, it never traps).
- **Events on the wire:** `On("click", "removeItem", "42")` renders as
  `data-on-click="removeItem" data-arg-click="42"` — two attributes, so
  neither name nor payload needs in-attribute delimiters beyond standard
  escaping.
- `document(title: String, head: Array<Html>, body: Html) -> String` builds a
  full `<!doctype html>` page (used by SSR and by M3's dispatcher).

**Deliverables:** `std/html.vyrn`; `examples/htmltree.vyrn` (parity citizen:
builds a tree with every variant, escaping corners, void elements, prints
`toHtmlString`); unit tests in `Program.tests`.

## M2 — `web/vyrn-dom.js`: the client runtime

Plain JavaScript beside `wasi-min.js`, no privileged access — it talks to
ordinary wasm exports.

- **Boot:** instantiate the client wasm, call the app's exported
  `vyrnView() -> String` (user-written, one line: `return toJson(view())`),
  parse, build DOM under the mount node.
- **Update:** after any handler returns, call `vyrnView()` again and diff the
  new tree against the retained old one — keyed diffing where `Key` attrs are
  present (reorder/move), positional otherwise. Patch the real DOM minimally.
  v1 re-renders the whole tree per event; the diff absorbs it. (M5 replaces
  this loop, not the surface.)
- **Events:** one delegated listener per event type on the mount root. On an
  event, walk to the nearest `data-on-<type>`, then invoke the exported extern
  handler by name. **Handler ABI (locked):** every handler is
  `export extern fn name(arg: String)`. For `click`/`keydown` the host sends
  the declared `data-arg-<type>` payload; for `input`/`change` it sends the
  control's current value; for `submit` it sends the payload (and calls
  `preventDefault`). Handlers parse/validate their own arg — the same
  boundary discipline as everything else.
- **Subscriptions (effects as data, the Elm answer):** the app may export
  `vyrnSubs() -> String` returning `toJson(subs())` where

  ```vyrn
  type Sub = | Every(Int64, String)        // interval ms, handler
             | Keydown(String, String)     // key, handler
  ```

  After each render the host diffs the declared list by value: appeared →
  wire, disappeared → unwire. Mount/unmount/cleanup fall out; there are no
  callbacks to leak. The vocabulary is deliberately tiny — it grows by
  demand, and third parties can define their own `Sub`-like types with their
  own runtime.
- **Escape hatch for imperative DOM** (focus, measure, third-party widgets):
  `A("data-effect", "mountChart")` — the host runtime exposes a registry
  (`vyrnDom.effect("mountChart", fn)`) invoked when such a node appears or
  disappears. Named, greppable, deliberately slightly uncomfortable.

**Deliverables:** `web/vyrn-dom.js`; a browser demo page (RFC-0013-style
manual verification: counter + keyed list reorder + an input + a
subscription); the fullstack demo's client migrated off string-built HTML.

## M3 — `std/ui`: the pages generator (routing)

`gen fn pages(dir: String) -> String` — the i18n pattern applied to a
directory of **ordinary `.vyrn` page modules** (M4 adds `.vyx` sugar; the
conventions below are the compile target either way).

```
pages/
  index.vyrn           → GET /
  items/index.vyrn     → GET /items
  items/[id].vyrn      → GET /items/:id
```

- **A page module exports** `fn page(p: Params) -> Html` — and, when it has
  dynamic segments, `type Params = { id: Int64, … }` whose fields must match
  the `[bracket]` segments (checked via `moduleInterface` at generation time;
  mismatch = load diagnostic pointing at the generator call). Optionally
  `fn load(p: Params) -> Validation<Data>` and then
  `page(p: Params, d: Data) -> Html`.
- **The synthesized module exports** `route(req: Request) -> Response`: match
  the path, parse+validate each dynamic segment against the param's schema
  (an `Int64` segment that isn't an integer never reaches user code — 404),
  run `load` (Invalid → a 422/500 error page built with `std/html`), render
  `page` through `document(...)`, return the `Response`. The user's `handle`
  is one line: `return route(req)`.
- **Typed URLs:** the generator emits `type RoutePath` — a regex-validated
  string type of the route language (static segments literal, an `Int64`
  param as `(0|-?[1-9][0-9]*)`) — plus one helper per dynamic route
  (`hrefItem(id: Int64) -> RoutePath`). Static hrefs are checked as string
  literals against `RoutePath` (existing machinery); dynamic hrefs go through
  the helpers, which are typo-proof by construction. Interpolation-containment
  for `"/items/\{id}"` (an `Int64` hole ⊆ the segment language) is attempted;
  if the containment checker can't see through `@str(Int64)` today, that's a
  finite-types gap to note, NOT to hack — helpers carry v1.
- **v1 is MPA:** every navigation is a full request and a server render —
  which host-owns-the-loop handles with zero client-router complexity. SPA
  takeover (history API + loaders through `web/vyrn-query.js`) is an M5+
  host-runtime feature over the same route table.
- **Layouts:** plain function composition (`fn shell(inner: Html) -> Html`,
  called by pages). A `layouts/` convention is deferred until real apps ask.

**Deliverables:** `pages` in `std/ui.vyrn`; the fullstack demo rebuilt as
`pages/` + `route(req)`; generation-failure tests (param/type mismatch,
colliding routes); `vyrn emit-gen` goldens.

## M4 — `.vyx` component files (the DX layer)

`gen fn components(dir: String) -> String` compiles single-file components to
plain view functions in a synthesized module. **Templates are the surface;
M1 hyperscript is their compile target** — the render-function relationship
Vue has, made inspectable via `vyrn emit-gen`.

```html
<!-- components/ItemRow.vyx -->
<script>
props { item: Item }
import { t } from "../i18n"
</script>

<template>
<li class="flex gap-2 p-2">
    <span class="font-bold">{item.title}</span>
    {#if item.count > 1}<span class="text-dim">×{item.count}</span>{/if}
    <button class="px-2 rounded bg-brand-500" @click="removeItem(item.id)">
        {t("cart.remove")}
    </button>
</li>
</template>
```

- **Template grammar (locked, Svelte-flavored — brace blocks compose with
  `{expr}` interpolation and Vyrn's expression grammar):**
  - `{expr}` — interpolation, always escaped (`Text`); `{@raw expr}` for `Raw`.
  - `{#if cond} … {:else if cond} … {:else} … {/if}` → nested `Empty`-elided
    conditionals.
  - `{#for x in expr key={expr}} … {/for}` → `map` + `keyed` (the `key` is
    required — keyed diffing is not optional in lists).
  - `<Capitalized prop={expr}/>` — component call (same dir or imported);
    children between tags arrive as a trailing `Array<Html>` parameter.
  - `@click="handler(expr)"` (and `@input`, `@submit`, `@keydown…`) —
    **exactly one root-exported handler name + at most one scalar
    (`Int64 | String | Bool`) argument expression**, compiled to
    `On(event, "handler", str(expr))`. Not arbitrary code — the M2 dispatch
    mechanism made invisible, honestly limited by module-state-is-root-only.
  - Everything else — attributes, static text — passes through checked.
- **What's checked at generation/compile time:** unknown component tags,
  prop names and types (props are just the compiled function's parameters),
  interpolation expressions (they land in generated Vyrn and hit the real
  checker, with the generator mapping diagnostics back to the `.vyx` line),
  `t(…)` keys (ordinary i18n imports in the script section), and — when the
  app imports a `Tw` theme type — every static `class` string.
- **The template compiler is written in Vyrn**, comptime-pure, in `std/ui` —
  the ICU parser's big sibling (byte-walker over the template section,
  brace-block parser, expression pass-through with position mapping).
- **`props {}` and nothing else.** No `state {}` (deferred — see M5+), no
  lifecycle, no slots-beyond-children, no scoped styles in v1.

**Deliverables:** `components` in `std/ui.vyrn`; the fullstack demo's pages
and components rewritten as `.vyx`; emit-gen goldens; generation-diagnostic
tests (bad prop, unknown tag, missing `key`, non-scalar event arg, unclosed
block — each names the `.vyx` file and line).

## M5+ — deferred, with the designs sketched so they aren't lost

- **Compiled reactivity (the Svelte bet).** The template compiler statically
  knows every binding's store/prop dependencies. Store writes flow through
  generated setters that flip dirty bits; the component compiles an
  additional `patch(dirty) -> Array<PatchOp>` that re-evaluates only affected
  bindings; the host applies targeted ops. No closures, no proxies, no vdom —
  the dependency graph lives where Vyrn puts everything: compile time.
  Replaces M2's render-and-diff loop without touching the surface.
- **SPA takeover:** history API + M3 loaders through `web/vyrn-query.js`
  (dedupe/staleTime/invalidate already exist), same route table client-side.
- **`state {}`** as generator sugar lowering to instance-keyed store entries
  (tree-path + `key` identity) — only if dogfooding proves lifted state too
  heavy, and only as visible-in-emit-gen lowering.
- **LSP embedded regions:** hover/completion *inside* `.vyx`. Must land as a
  format-agnostic mechanism (generators expose source-map/region info) so
  third-party formats get identical treatment — the one place a first-party
  framework could accidentally acquire an unfair advantage.
- **`std/tw`:** `tw("./theme.json")` emitting a finite `Tw` class type +
  `css() -> String` served by a route. Small, independent, slots into M4's
  class checking; its own mini-RFC when picked up.
- **Streaming SSR, layouts/ convention, richer `Sub` vocabulary:** by demand.

## Testing & parity story

- `std/html` and all view code: pure → parity citizens (`examples/htmltree.vyrn`
  three-way; component snapshots via `assertEq(toHtmlString(…), "…")` in
  `vyrn test`).
- Generators: `vyrn emit-gen` goldens + load-diagnostic tests, the std/rpc
  pattern.
- `web/vyrn-dom.js`: browser-verified demo (the RFC-0013 protocol) — diff
  correctness exercised by a keyed-reorder page; not in the parity harness
  (host JS, like `wasi-min.js`).

## Out of scope

Runtime signals, scoped CSS, CSS-in-Vyrn, animations/transitions, streaming
SSR, islands/partial hydration, a client router (v1 is MPA), accessibility
lint, devtools. And — by the prime constraint — any compiler change at all.
