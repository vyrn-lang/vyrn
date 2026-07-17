# RFC-0036 — `.vyx` ↔ `Tw`: Compile-Checked Classes in Templates

- **Status:** Draft (design locked)
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
