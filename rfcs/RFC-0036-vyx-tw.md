# RFC-0036 — `.vyx` ↔ `Tw`: Compile-Checked Classes in Templates

- **Status:** Implemented. See the as-landed notes at the end.
- **Depends on:** RFC-0032 (`std/tw` — the `Tw`/`TwClass` types and `cls`),
  RFC-0026 M4 (`std/vyx` — the `components` generator emitting `class=`
  attributes), RFC-0033 (origin maps — so a class typo lands in the
  `.vyx` at the right column), RFC-0027 (`import * as ns` — the synthesized
  module imports the theme namespaced)
- **Evidence:** RFC-0032 and RFC-0033 both explicitly deferred this,
  naming the blocker: "how does `components(dir)` learn the theme?".
  shelf's server chrome is `Tw`-checked (RFC-0032) but its `.vyx` client
  components still use unchecked `class="…"` strings — a typo there is
  invisible until the browser. This closes the last checking gap in the
  UI stack.

---

## The coupling: an explicit theme argument

`components` learns the theme the same way it learns anything — an
argument, resolved (like `dir`) relative to the importing module:

```vyrn
import { componentsThemed } from "std/vyx"
import { Row, Panel } from componentsThemed("./components", "./theme.json")
```

- **`components(dir)` is unchanged** — no theme, `class="…"` emits
  unchecked `Cls("…")` exactly as today. Zero churn for existing callers;
  this is purely additive.
- **`componentsThemed(dir, theme)`** (the two-argument form — a distinct
  generator entry, since it changes emission) threads the theme through:
  the synthesized module gains `import * as vyxTheme from tw(<theme>)`
  (a nested generator import — RFC-0033's nested-path fix already makes
  this resolve), and every `class` attribute is emitted through
  `vyxTheme.cls(…)` instead of bare `Cls(…)`.

(The implementer may instead express this as one generator that treats a
second argument as optional if the language allows it — report the
mechanism chosen. The *surface contract* above is what's locked.)

## What emission becomes

- **Static class — compile-checked.** `class="flex gap-2 p-4"` →
  `vyxTheme.cls("flex gap-2 p-4")`. The literal is a `String` argument to
  `cls(c: Tw)`, so the existing consteval containment machinery (RFC-0032)
  proves it `⊆ Tw` at compile time; a typo (`flx`, `gap-77`) is a
  `vyrn check` error. **Origin fidelity upgrade:** a static `class`
  attribute gets its own column-exact `//@origin` directive (today it is
  emitted inline on the element's push line → region-level), so the `Tw`
  error lands on the offending class string inside the `.vyx`, not on the
  element head. This is a concrete RFC-0033 producer improvement.
- **Dynamic / interpolated class — runtime-validated.** `class={expr}`
  and `class="flex {dyn}"` (mixed static+interpolation) → `vyxTheme.cls(
  <expr>)`. A `String` coerces to `Tw` at the call boundary — the
  standard validated-type runtime check (RFC-0032's runtime-string
  story), trapping on an invalid class with the canonical wording. v1
  does NOT attempt to statically prove the static *prefix* of a mixed
  class; the whole attribute is one runtime-checked `Tw` value. (Fully
  static is the compile-checked path; any interpolation drops to runtime.)

## The safelist: utilities and bespoke classes coexist (std/tw)

Real templates carry non-utility class names (`book-card`, `row`, a
third-party widget's classes). Under a checked `Tw` those would be
compile errors, which is too strict. `theme.json` gains an optional
**`safelist`** — an array of literal class names folded verbatim into the
`TwClass` vocabulary:

```json
{ "colors": {…}, "spacing": [...], "safelist": ["book-card", "row", "prose"] }
```

- Safelisted names join `TwClass` (so `class="book-card p-4"` checks),
  validated to the same CSS-safe shape (`[a-z][a-z0-9-]*`, documented; a
  bad name fails generation naming the key).
- **`css()` emits NO rule for a safelisted name** — it only asserts the
  name is *valid to reference*; the app owns its styling in a bespoke
  stylesheet. (This is precisely Tailwind's safelist semantics.)
- Ordering: safelist entries emit into `TwClass` after the derived
  vocabulary, source order — the byte-stability rule (RFC-0032) extends.

## Consumers / proof

- **shelf** switches its `.vyx` client components to
  `componentsThemed("./components", "./theme.json")`, moving its bespoke
  class names (`book-card`, etc.) into the theme's `safelist`; the utility
  classes in those templates are now compile-checked. A deliberately
  typo'd class in a test `.vyx` fails `vyrn check` with the `Tw`
  diagnostic pointing at the `.vyx` line:col (the origin-map proof).
- Browser-verify shelf unchanged end to end (styles identical — the
  emitted `Cls` values are byte-identical, `vyxTheme.cls` just adds a
  compile-time gate).
- `std/vyx` golden/emit-gen update for the themed emission shape; an
  in-language test that a safelisted + utility mix checks and an unknown
  class does not.

## Out of scope

Per-class arbitrary-value checking (still none), `dark:` and other
RFC-0032-deferred variants, static checking of a mixed static+interpolation
class's literal prefix, a `Tw`-typed prop threaded between components
(components pass `class` as ordinary `String`/`Tw` params — no new prop
machinery), used-only CSS pruning (unchanged from RFC-0032).

---

## As landed

Shipped as generator-library work only — **ZERO compiler/CLI changes** (touched
`std/tw.vyrn`, `std/vyx.vyrn`, `examples/shelf`, and two CLI test files). The
consteval containment machinery (RFC-0032) sees the class literal straight
through the namespaced `vyxTheme.cls("…")` call, exactly as the server chrome's
`theme.cls("…")` already did — no checker change was needed or made.

**`componentsThemed` mechanism (chosen: a distinct generator entry).** A second
`export gen fn componentsThemed(dir, theme)` sits beside `components(dir)`; both
run the same `.vyx` compile loop and share the pure assembly tail
(`vyxFinish` → `vyxBuildModule(comps, themed, theme)`). A distinct entry (not an
optional second argument) is the honest choice: it changes emission, and the
synthesized module's key already dedups per generator-name + args (RFC-0021), so
themed and non-themed builds of the same dir never collide. `components(dir)`
stays **byte-identical** — bare `Cls(…)`, no theme import. One constraint
surfaced: `listDir`/`readFile` are comptime-only builtins with no native/wasm
lowering, so they may appear only **directly inside a `gen fn` body** — the I/O
loop is therefore duplicated in the two generators (not hoisted into the shared
plain-`fn` helper), which native parity caught immediately.

**Theme import resolution (verified).** The themed module emits
`import { tw } from "std/tw"` + `import * as vyxTheme from tw(<theme>)`. The
nested generator import resolves the theme path against the **real importing
file's** directory, not the synthetic banner — the loader already unwraps
`generated_importer(importer)` for exactly this (RFC-0021/RFC-0033). So
`componentsThemed("./widgets", "./theme.json")` in `examples/shelf/client.vyrn`
makes the synthesized module read `examples/shelf/theme.json`. Confirmed end to
end: the themed shelf client builds to wasm and renders.

**Static-checked vs runtime-validated split (as emitted).** A **static**
`class="…"` is hoisted onto its own line — `let <k>_cls: Attr =
vyxTheme.cls("…")` — so the `String` literal is proven `⊆ Tw` at compile time; a
**dynamic** `class={expr}` stays inline as `vyxTheme.cls(<expr>)`, a
`String→Tw` coercion checked at the runtime boundary (shelf's `TagItem`
`class={cls}` with `cls = "tag active"` was exercised live — the coercion
succeeds because both names are safelisted, no trap). Emitted `Attr` values are
byte-identical to the bare build (`vyxTheme.cls(c)` returns `Cls(c)`).

**Origin-fidelity upgrade (column-exact, proven).** The hoisted static-class line
carries its **own** `//@origin <file>:<line>:<col>` pointing at the class
string's exact column in the `.vyx` (previously attributes were inline on the
element push line → region-level only). RFC-0033 `remap` relocates a `Tw`
consteval error to that directive's `file:line:col`. Proven by
`compiler/vyrn-cli/tests/vyx.rs::themed_typo_class_remaps_to_the_vyx_column`: a
typo'd `class="flx"` on `Widget.vyx:2` reports at **`Widget.vyx:2:12:`** (column
12 = the `f` of `flx`), with the generated location kept as an `emit-gen` note.

**Safelist semantics (std/tw).** `theme.json` gains an optional `safelist` array
(the JSON reader learned to parse a string array, flattening it to
`safelist.<index>` axis entries in source order). Each name is validated to the
same `[a-z][a-z0-9-]*` shape (a bad name fails generation as
`TW_UNSAFE_SAFELIST__<name>`), then folded into the **`TwClass` token grammar's
base alternation after** the derived vocabulary — so `class="book-card p-4"`
checks — while `css()` is left untouched: **no rule is emitted for a safelisted
name** (Tailwind's safelist semantics). `safelist` also joins the recognised
top-level keys so it is not an unknown-key error. `/theme.css` stays
byte-identical (60616 bytes on the shelf theme).

**shelf's safelist (27 names).** Every static bespoke class across the client
`.vyx` components plus the two runtime values of `TagItem`'s dynamic `cls`:
`card, cardbody, rating, tags, issues, book, meta, title, detail, row, rate,
danger, shelf, bar, count, lang, noissues, cols, col-main, books, empty,
sidebar, taglist, tag, active, add, save`. The class strings in the `.vyx` are
unchanged (styles identical); `client.vyrn` now imports
`componentsThemed("./widgets", "./theme.json")`. A negative test `.vyx` (typo'd
utility) lives in `vyx.rs`, NOT the parity corpus.

**Browser evidence (`vyrn dev`, shelf).** All pages render with identical
styling. Header utilities resolve from `/theme.css` (`display:flex`,
`gap:24px` from `md:gap-6` at ≥768 px, `mr-2`→8 px); safelisted bespoke resolve
from `public/style.css` (`li.book` computes `display:flex`, 1 px bottom border);
the wasm client hydrates `#app` with the full `.vyx` component tree
(`shelf/bar/count/…/book/meta/title/detail`); clicking a tag filter flips its
class to `tag active` with no trap; console clean throughout; `/theme.css` is
60616 bytes and contains no rule for any safelisted name.

**Tests / parity.** Workspace 847 → **850** (+3 CLI tests in `vyx.rs`: themed
typo remap, safelist+utility run, themed emit-gen shape); `std/tw` unit tests
11 → 14, `std/vyx` unit tests +2; LSP 11 green; full three-way parity green
(`components(dir)` byte-identical, `twdemo`/`vyxdemo` unaffected). Zero
warnings.
