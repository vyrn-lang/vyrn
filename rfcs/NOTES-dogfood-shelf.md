# Dogfood notes — building `examples/shelf/` on the full Vyrn stack

A record of every point of friction found while building **shelf** (a small
library/bookmark manager) end to end on `std/html` + `std/ui` (pages) + `std/rpc`
+ `std/i18n` + `Map` + `import * as ns` + `.vyx`, served by `vyrn dev` and
browser-verified. The app is the pretext; this report is the deliverable.

The app works — every page, the dynamic route (incl. 404 / non-integer / loader
422), add / delete / rate with re-render, a server 422 surfacing accumulated
Issues, en⇄uk locale switch with CLDR plurals, and live tag-count `Map`s — but
getting there required **two hard compiler bugs to be worked around** and a
**structural wall (root-only state)** to be designed around. Both hit *because
shelf is the first program to combine these libraries in one build*; the existing
`fullstack` demo sidesteps everything by being 100% stateless.

---

## TL;DR — top 5 friction items

1. **Root-only module state makes `std/rpc` and `std/ui` pages stateless-only.**
   The RPC contract module and page modules are both *imported* (non-root), so
   neither can touch the app's `books` store. `rpcServer` is unusable; I
   hand-wrote the entire server RPC dispatch in the root. (language, structural)
2. **BUG (FIXED, ef7522c): flat-namespace resolver ignores local shadowing.** A
   local/param/loop var whose name matches *another linked module's* top-level
   export was mis-read as an un-imported cross-module reference. This made
   `std/i18n` un-linkable with `std/html` (`on`) and `std/ui` (`t`). Hit 4×.
   The visibility scan is now scope-aware. (compiler)
3. **BUG (FIXED, b3a07c6): generator cache collides on the arg string alone.**
   `rpcClient("./api")` from shelf served *fullstack's* `api.vyrn`. The cache key
   omitted the importer dir and resolved inputs; it now folds the resolved input
   roots in. (tooling)
4. **i18n cannot be used inside a `.vyx`, and pre-generating it forfeits the
   locale state carve-out.** All labels resolve in the root and pass as props
   (prop explosion); `setLocale` had to be stripped and `Locale` threaded by hand.
   (generator + language)
5. **Validated request types can't be constructed invalid on the client**, so a
   typed stub can never send a payload that triggers a server 422 — you must drop
   to raw wire. (language / RPC ergonomics)

---

## HARD BUGS (wrong behavior — reported, worked around in-app with marked comments)

### BUG 1 — Generator output cache collides across importers with the same arg string

> **FIXED** (b3a07c6): `generator_sources_hash` now folds the RESOLVED input
> roots (the arg paths rebased onto the importing module's directory) into the
> cache key, so two importers in different dirs with the same relative arg no
> longer share an entry. Pre-fix cache entries self-heal (key-format change =
> clean miss). Shelf's globally-unique generator args are no longer required.


`compiler/vyrn-frontend/src/loader.rs::generator_sources_hash` (~line 702) keys
the on-disk cache (`~/.vyrn/cache/gen`) on **generator source ++ generator name ++
the literal arg string**. It does **not** include the importer directory or the
resolved input path. So two modules in different directories that both write
`rpcClient("./api")` (or `pages("./pages")`, `i18n("./locales")`,
`components("./components")`) produce the **same** cache key. The validation step
(5a) then re-hashes the *first* importer's recorded input paths (still on disk,
unchanged) → all match → false hit → the wrong module is served.

Reproduced deterministically: shelf's client importing `rpcClient("./api")`
emitted **fullstack's** contract (`Age`, `Username`, `User`, …) with the cache
warm, and shelf's own contract only with `VYRN_NO_GEN_CACHE=1`.

Blast radius across the corpus (every generator arg that repeats across dirs):
`pages("./pages")` (fullstack, pagesdemo, std/ui doc), `i18n("./locales")`
(i18ndemo), `rpcClient("./api")` (fullstack), `components("./components")`
(fullstack). Any real app using conventional directory names collides
non-deterministically with whatever the cache saw first.

**Workaround in-app:** shelf uses globally-unique generator args —
`rpcClient("./contract")`, `pages("./routes")`, `i18n("./strings")`,
`components("./widgets")` — so it can never share a key with another example.
That is *why* the directories aren't named `api`/`pages`/`locales`/`components`.

**Fix direction:** fold the importer dir (or the sorted resolved input paths)
into `sources_hash`.

### BUG 2 — Flat-namespace resolver mis-resolves a local as an un-imported foreign export

> **FIXED** (ef7522c): the link-time visibility scan is now scope-aware — it
> binds params, `let`, `for`/`while`/lambda vars and match binds before checking
> foreign references (a scanner modeled on the RFC-0027 `NsResolver` walk), so a
> local shadowing a foreign export is never mistaken for an un-imported
> reference. The shelf renames (`loc`/`cls`/`t`) are removed and the app links
> `std/html` + `std/ui` + the i18n module cleanly.


A **local variable, parameter, or loop binding** whose name equals a **top-level
export of another linked module** is reported as
`function \`X\` is defined in \`M\` but not imported here — add it to an import list`,
even though it is plainly a local. No existing example triggers it because no
example previously *co-linked* the offending pairs. Shelf hit it **four times**:

| local (in module)                     | collides with export        |
|---------------------------------------|-----------------------------|
| `std/i18n.vyrn` local `on`            | `std/html`'s `on()`         |
| `std/ui.vyrn` loop var `t`            | i18n's exported `t()`       |
| shelf `client.vyrn` top-level `loc`   | `strings_gen`'s param `loc` |
| shelf local `cls` / `.vyx` prop `cls` | `std/html`'s `cls()`        |

Consequence: **`std/i18n` is currently incompatible with the entire UI layer** —
importing the `i18n` generator links `std/i18n.vyrn` (local `on`) alongside
`std/html` (exported `on`), which fails to check.

**Workarounds in-app (all marked in source):**
- **Pre-generate the i18n module** to `strings_gen.vyrn` (a checked-in app file)
  so `std/i18n.vyrn` never enters the runtime link → kills the `on` collision.
- **Rename the pre-gen module's exported `t()` → `tr()`** to dodge `std/ui`'s
  local `t`.
- Rename my own `loc` → `lang` and `cls` → `tagCls` / `.vyx` prop `cls` → `klass`.

Short, common identifiers (`t`, `on`, `cls`, `id`, `el`, `loc`) are landmines:
any of them as a local anywhere in the program breaks if *any* linked module
exports the same name. `std/html` exporting one-letter-ish names (`el`, `on`,
`cls`) makes this especially sharp.

**Fix direction:** name resolution must bind locals/params/loop vars first; a
resolved local can never be a cross-module reference. (This is squarely the
RFC-0027 flat-namespace disease, one level below imports.)

---

## STRUCTURAL WALL — root-only module state (the #1 finding)

Module state is root-only (RFC-0013): only the entry module of a command may hold
a top-level `let`. But:

- the **RPC contract module** is *imported* by `rpcServer`/`rpcClient` → non-root
  → cannot hold the `books` store; and
- every **`std/ui` page module** is *imported* by `pages()` → non-root → cannot
  read the store either.

So the store is reachable from **exactly one place: the root's own `handle`** (and
functions the root calls locally). Every stack pillar that routes through a
*generated consumer module* is therefore **stateless-only**. `fullstack` hides
this by deriving everything from the request (`getUser(id) = "user{id}"`).

What that forced in shelf:

- **`rpcServer` is unusable.** It would call the contract's procedure bodies,
  which can't see the store. So `server.vyrn` **hand-writes the whole server RPC
  dispatch** (`POST /rpc/<name>` → `fromJson` with the contract's validated types
  → store op → `toJson`), honoring the exact wire shape `rpcClient` expects (200 =
  `toJson(result)`; 422 = `{"issues":[...]}`). This is precisely the code
  `rpcServer` would have generated — reimplemented by hand.

  ```
  // what I wanted:
  import { rpcHandle } from rpcServer("./contract")   // ← can't: procedures are stateless
  // what I wrote: ~40 lines of hand dispatch in the root, over module state
  ```

- **The contract's procedure bodies are dead placeholders.** They exist only so
  `rpcClient` can reflect each procedure's *shape*; the real impls live in the
  root. `contract.vyrn`'s `listBooks`/`addBook`/… all `return Err("contract stub
  …")`. A real developer resents writing bodies that are never run.

- **`std/ui` pages can't render store data.** `pages("./routes")` serves only the
  stateless shell / about / detail-shell routes; the live list, detail record, and
  tag sidebar are all fetched by the **browser client over RPC**. The task's
  "index page renders the list" is *not expressible* as an SSR page — the page
  module can't read the store.

**Fix direction (the big RFC):** a sanctioned way for a generator-consumer module
to reach app state — e.g. a context value threaded by the router/rpc dispatcher,
or `export let` + an accessor protocol, or extending the RFC-0021 state carve-out
to the modules these generators import. Without it, `std/rpc` server-side and
`std/ui` data pages are toys.

---

## GENERATOR FRICTION (.vyx / pages / rpc / i18n)

### i18n is unusable *inside* a `.vyx`
A generator import nested in a `.vyx` script —
`import { tShelfCount } from i18n("../strings")` — rebases the path but the
**nested generator** (a generated components module importing another generator)
resolves it wrong and fails with
`I18N_ERROR__no_locale_json_files_found`. So **no component can localize itself**;
every label is resolved in the client root and passed as a prop. With i18n unusable
in `.vyx`, the `Labels` record grew to 14 fields threaded through every widget.

### Pre-generating i18n forfeits the state carve-out
The generator's `setLocale`/`currentLocale`/`locale()` only work because the
*synthesized* module gets the RFC-0021 state carve-out. Once pre-generated to a
checked-in file (to dodge BUG 2), it's an ordinary non-root module →
`module state is root-only`. I had to **strip `setLocale` and thread `loc: Locale`
through all ~26 accessors and every view function**. The ergonomic
`setLocale(Uk); t("…")` became `tShelfCount(lang, n)` everywhere, with `lang` held
as root state in each of the two builds.

### `.vyx` duplicate type imports
Importing the same type in ≥2 component scripts emits a duplicate import into the
one synthesized module: `\`Labels\` is imported twice into this module`. Forced a
design where **only `ShelfApp.vyx` imports the record types** and all child
widgets take **scalar props only** (hence `TagItem(name, label, count, klass)`
instead of `TagItem(t: TagCount)`), inflating call sites.

### Locked one-scalar-arg event ABI
`@click="h(scalar)"` allows exactly one scalar. Rating a specific book needs
`(bookId, star)` — two values — so the star widget became a single **cycle-rate**
button carrying just the id (`0/5→1→…→5→1`). Fine, but a per-star control would
need `"id:star"` string-encoding and manual parsing.

### Validated types can't be *constructed* invalid → no client-driven 422
Building `AddBookReq { title: "" }` **traps** client-side (validation), so a typed
stub can never carry an invalid payload to exercise a real server 422. Two honest
consequences:
- the add form validates via `fromJson(AddBookReq, toJson(rawDraft))` (which
  returns `Invalid` instead of trapping) and surfaces those Issues locally;
- a *genuine* server 422 is demoed from `app.js` posting a raw bad body straight
  through the transport (the `fullstack` pattern) — because there is no in-language
  way to send one.

---

## LANGUAGE-SURFACE FRICTION (smaller, mechanical)

- **No `match` on `Bool`, and `if` is not an expression.**
  `match cond { true => a, false => b }` is rejected (`expected identifier, found
  True`); with no `let x = if …`, every ternary is a `let mut x = b; if cond { x =
  a }`. Multiplied across the client's view code.
- **`Map` alias not inferred for `[:]`.** `let m: TagCounts = [:]` fails (`cannot
  infer the type of [:]`); must spell `let m: Map<String, Int64> = [:]` even though
  `TagCounts = Map<String, Int64>`.
- **Validated field element-store needs a pre-typed value.** `books[i].rating =
  rating` (an `Int64`) is rejected (`assign an already-constructed Stars value`);
  needs `let s: Stars = rating; books[i].rating = s`.
- **No structural copy.** Returning a value out of the store without moving module
  state means hand-writing `copyBook` (and a manual tag-array copy loop).
- **Enum variants can't be imported by name**, only the enum type — and the
  diagnostic is contradictory: `strings_gen does not define \`En\`` *and* `\`En\`
  is defined in strings_gen but not imported here` for the same identifier.

## STD LIBRARY GAPS

- **`std/strings` has no `startsWith` / `substring` / `split` / `contains` /
  `trim` / `indexOf`.** A path router (`/rpc/<name>`) and a `"a, b, c"` tag parser
  both had to byte-walk by hand — I wrote `util.vyrn` (`hasPrefix`, `dropPrefix`,
  `trim`, `splitTrim`). These are obviously general and belong in `std/strings`.

## TOOLING

- **`vyrn dev`** is genuinely smooth: one command built the client to wasm, served
  `public/`, mounted the runtimes, and routed `/rpc/*` to `handle`. No complaints.
- **`--target wasm` env discovery** is fiddly: `WASI_BUILTINS` must point at the
  `.a` *file* (not its directory), and the error text doesn't say so. `vyrn dev`
  handled it once the env was set.
- **Diagnostics** are mostly excellent (validated 422 Issues are a joy). The two
  bug classes above produce *misleading* diagnostics, though — a genuine
  name-resolution bug masquerades as "you forgot an import," which sent me hunting
  the wrong thing first.

---

## WHAT WORKED WELL (do NOT change these)

- **`Map<String, Int64>` end to end.** Folded on the server, encoded as a JSON
  object in insertion order (`{"reference":2,"web":1,"fiction":1}`), decoded on the
  client, iterated via `keys()`. Zero friction, byte-clean. The map pillar is
  solid.
- **`import * as api` over a generator import.** `api.listBooks()`,
  `api.addBook(api.AddBookReq { … })`, `Validation<api.BookList>`, and
  `fromJson(api.AddBookReq, …)` all worked first try across expression, type, and
  record-constructor positions. RFC-0027 delivers.
- **`rpcClient` generation.** Typed stubs + per-procedure completion dispatchers +
  verbatim re-emitted contract types, including **zero-param procedures**
  (`listBooks()`), `Result` payloads, and `Map` returns. The `(id, res)` handler
  shape is uniform and predictable.
- **The accumulating validation model.** One bad body produced three path-tagged
  Issues (`title` / `url` / `tags[0]`) rendered verbatim in the UI. Excellent, and
  the same Issue vocabulary works client- and server-side.
- **`vyrn-dom` keyed diffing.** Rate/add/delete re-renders preserved the add-form's
  input focus and reordered/removed keyed rows correctly. The TEA loop
  (state → `view()` → JSON → diff → patch) is dependable.
- **CLDR Ukrainian plurals** (one/few/many) generated correctly and rendered both
  server-side (About page) and client-side (`4 книги` few → `5 книг` many).
- **`pages` router error paths.** 404 for a non-integer segment and the 422 error
  page from a loader `Invalid` worked exactly as designed, no glue.
- **`.vyx` templates**, once the constraints were internalized:
  `{#if}/{:else}/{#for … key={}}`, nested component composition, scalar props, and
  attribute pass-through all compiled and rendered. `emit-gen` visibility helped.
- **`vyrn test`** with `test` blocks excluded from `build`/`serve`, including a
  **view snapshot of a `.vyx` component** via `toHtmlString`.

---

## Prioritized next-RFC candidates

| # | Candidate | Evidence in shelf | Scope | Kind |
|---|-----------|-------------------|-------|------|
| ~~P0~~ DONE | **Name resolution binds locals before foreign exports** (ef7522c) | BUG 2, hit 4×; blocked i18n+UI entirely | small, high-value | compiler |
| ~~P0~~ DONE | **Generator cache key includes importer dir / resolved inputs** (b3a07c6) | BUG 1; wrong module served | small | tooling |
| P1 | **State access for generator-consumer modules** (context param, `export let`, or extend the carve-out) | rpcServer unusable; pages can't read the store; contract bodies are dead | large | language |
| P1 | **i18n usable inside `.vyx`** (fix nested-generator path resolution) | every label passed as a prop; 14-field `Labels` | medium | generator |
| P2 | **Pre-generation keeps locale state** (carve-out for checked-in generated modules) OR a stateless-by-design i18n surface | had to strip `setLocale`, thread `loc` everywhere | medium | language+lib |
| P2 | **Richer `.vyx` event ABI** (multiple / structured args) | rating had to become a cycle button | medium | generator |
| P2 | **`.vyx` dedup type imports across scripts** | forced scalar-only child props | small | generator |
| P3 | **`std/strings`: startsWith/substring/split/contains/trim/indexOf** | hand-wrote `util.vyrn` | small | library |
| P3 | **`if` as an expression / `match` on `Bool`** | every ternary is `mut`+`if` | medium | language |
| P3 | **Map-alias inference for `[:]`; validated element-store coercion; structural copy** | mechanical papercuts throughout | small each | language |
| P3 | **Clearer diagnostics** for enum-variant imports and (esp.) the BUG-2 shadowing case | misleading "not imported" errors | small | tooling |

---

## Verification summary

- Browser (via `vyrn dev`, port 8091): home SSR + client mount; en⇄uk toggle
  (labels + `4 книги`/`5 книг` plurals + localized tag labels); rate (unrated→★1,
  persisted); add (count few→many, tag `Map` recomputed); two-step delete confirm;
  client tag filter; **server 422** with three accumulated Issues in the panel;
  `/books/0` detail hydrated via `getBook`; `/books/999` loader 422 page;
  `/books/abc` 404; `/about` server-side plurals. No console errors.
- In-language tests: `vyrn test server.vyrn` (4/4 — tag folding + Map order,
  listBooks dispatch, invalid-add 422 without mutation, CLDR plurals);
  `vyrn test client.vyrn` (3/3 — `splitTrim`, `parseId`, a `.vyx` view snapshot).
- The app is a subdirectory of `examples/`, so the parity harness (top-level
  `examples/*.vyrn` only) never picks it up — same exclusion convention as
  `examples/fullstack/`. No `std/` or `compiler/` files were modified.
