# RFC-0052 — Safelisted Class Hover Shows the App's Own CSS

- **Status:** Implemented
- **Depends on:** RFC-0032/0036 (`std/tw`, the theme `safelist`), RFC-0042
  (class hover — utility classes already show their generated rule),
  RFC-0041 (`head { stylesheet … }` — how an app declares its CSS)
- **Evidence (user):** hovering `plang` in a `.vyx` returns
  `plang — safelisted (app-styled)`, which says nothing useful. `plang`
  *is* safelisted (one of bin's 24 bespoke names) and its rule is right
  there in `examples/bin/public/style.css`
  (`li.paste .plang { font-size: 0.72rem; … }`), declared by the layout's
  `head { stylesheet "/style.css" }`. A utility class hovers with its
  generated CSS; a safelisted one should hover with the app's CSS —
  otherwise the safelist is a second-class citizen in the editor.

---

## Behaviour

Hovering a **safelisted** class in a themed `.vyx`/`.vyrn` shows the
matching rule(s) from the app's own stylesheet, in the same shape the
utility hover uses:

```
**`plang`** — safelisted (app-styled)

```css
li.paste .plang {
  font-size: 0.72rem; text-transform: uppercase; letter-spacing: 0.04em;
  padding: 0.05rem 0.4rem; border-radius: 5px;
  background: color-mix(in srgb, currentColor 12%, transparent);
}
```
— public/style.css:24
```

- The "safelisted (app-styled)" line **stays** (it is the honest fact:
  `std/tw` generates nothing for this name); the rule is added beneath it,
  with its source file and line.
- **No match found** → today's text unchanged (the name is valid but the
  app hasn't styled it — worth knowing, not an error).
- **Multiple matches** (e.g. a base rule plus a `:hover`) → show them in
  file order, capped (say 3 rules / ~40 lines) so the tooltip stays a
  tooltip.

## Finding the stylesheet(s)

In priority order, from the `.vyx`/`.vyrn`'s app root (the RFC-0049
`app_root_for` walk — `vyrn.json`, else the generator-importing root):

1. **Declared**: stylesheet URLs from `head { stylesheet "…" }` blocks in
   the app's layout/page `.vyx` files, mapped URL→file by the serve
   convention (`/style.css` → `<app root>/public/style.css`). This is the
   precise answer and matches what the browser actually loads.
2. **Fallback**: any `*.css` under `<app root>/public/` (and the app root
   itself), so an app that serves CSS another way still works.

Read lazily and cache per app root, invalidated on change (the same
cheap-signature approach RFC-0049 §2 uses) — a tooltip must not re-read
the disk each hover.

## Matching a rule (pragmatically, and honestly)

No CSS parser. Scan selector blocks and keep those whose selector
mentions the class as a **whole token** — `.plang`, but not `.plangs`
or `.plang-x`; handles descendant/compound selectors
(`li.paste .plang`, `.plang:hover`, `a.plang`). Report the block verbatim
with its file:line. This is a heuristic tooltip, not a semantic CSS
model — documented as such, and it degrades to "no match" rather than
guessing.

## Verification

- Drive the **deployed** binary (VS Code URI form `file:///n%3A/…`) and
  probe `class="plang"` in `examples/bin/routes/index.vyx`: before =
  `plang — safelisted (app-styled)`; after = same line **plus** the
  `li.paste .plang { … }` block and `public/style.css:24`. Also probe a
  safelisted name with **no** rule (unchanged text) and a **utility**
  class (unchanged generated-CSS behaviour — no regression).
- LSP e2e tests for: safelisted-with-rule, safelisted-without-rule,
  utility unchanged, and the whole-token match (a `.plang-x` rule must
  NOT match `plang`).
- Full suite + LSP green, 0 warnings; parity unaffected (editor-only).
- **Rebuild + HASH-VERIFIED redeploy** (fresh == deployed, both reported).

## Out of scope

A real CSS parser / cascade resolution (which rule actually wins),
`@media`/`@supports` context beyond showing the block, CSS-in-`.vyx`
`<style>` blocks (none exist), authoring features (completion of
safelisted names already works), and hovering classes in plain `.css`
files.

---

## As landed

Editor-only: everything lives in `compiler/vyrn-lsp/src/main.rs` (the excluded
crate). No frontend, compiler, emitted-code or parity change — `class_token_hover`
is untouched; `handle_hover` post-processes its result through `with_app_css`.

**Trigger.** `with_app_css` fires only when the hover text ends with
`— safelisted (app-styled)` (the class name is read back from the
`` **`…`** `` prefix). A utility hover, or any non-class hover, is returned
byte-identical — that is what keeps the no-regression guarantee cheap.

**Discovery order** (from `app_root_for(dir of the hovered file)`, the RFC-0049
walk):

1. **Declared** — every `.vyx` under the app root (recursive, skipping
   hidden/`vyrn_vendor`/`target`/`node_modules`/`public`, ≤64 files) is scanned
   textually for `stylesheet "…"` lines; each URL maps `<root>/public/<url>`,
   falling back to `<root>/<url>`. Files in that order, deduplicated.
2. **Fallback** — only when step 1 yields nothing that exists: every `*.css`
   directly under `<root>/public/`, then `<root>/`, name-sorted.

**Cache key.** One entry per app root, holding the read files. Its signature is a
hash of `(path, len, mtime)` for each cached stylesheet plus the `stat` of the app
root and `<root>/public/` directories — so an edit to a sheet *and* the addition
or removal of one both invalidate it, while a hover costs a handful of `stat`s and
no reads or directory walks.

**Matching heuristic.** Brace- and `/* … */`-aware block scanning, no CSS parser.
A block is kept when its selector contains `.<class>` not followed by a class-name
character (`[A-Za-z0-9_-]`): `li.paste .plang`, `.plang:hover`, `a.plang` match;
`.plangs` and `.plang-x` do not. At-rule bodies (`@media { … }`) are descended into
so inner rules are found. Deliberately **not** done: cascade/specificity ("which
rule wins"), showing the enclosing at-rule condition, `@import` following,
preprocessor sources, or classes composed at runtime. Rules are shown verbatim in
file order, capped at 3 rules / 40 lines.

**Before/after** (deployed `vyrn-lsp.exe`, VS Code URI form
`file:///n%3A/lang/examples/bin/…`):

| probe | before | after |
| --- | --- | --- |
| `plang` in `routes/index.vyx` (safelisted, styled) | `**`plang`** — safelisted (app-styled)` | same line **+** the `li.paste .plang { … }` block **+** `— public/style.css:24` |
| `backlink` (safelisted, unstyled — unsaved buffer edit) | `**`backlink`** — safelisted (app-styled)` | unchanged |
| `mr-2` in `routes/p/[id].vyx` (utility) | `.mr-2 {margin-right:0.5rem}` | unchanged |
| `hover:text-brand-600` (variant utility) | `.hover\:text-brand-600:hover {color:#1d4ed8}` | unchanged |

**Tests.** Three LSP e2e tests (`rfc52_safelisted_hover_shows_the_apps_own_css`,
`…_without_a_rule_is_unchanged`, `rfc52_utility_hover_is_unchanged`) cover the
rule append with `file:line`, whole-token rejection of `.book-card-x` /
`.book-cards`, discovery order (an undeclared decoy stylesheet is never
consulted), the unchanged no-match text, and the unchanged utility hover.
926 workspace + 35 LSP tests (1 ignored) green; parity green; rebuilt and
hash-verified redeploy.
