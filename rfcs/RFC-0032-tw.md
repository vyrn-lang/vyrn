# RFC-0032 — `std/tw`: Theme-Derived Utility Classes as a Checked Type

- **Status:** Draft (design locked)
- **Depends on:** RFC-0021 (generator imports), RFC-0003/0020 (validated /
  regex string types + consteval literal checking — the machinery that
  makes a typo a compile error), RFC-0026 (`std/html`/pages — where the
  classes and the stylesheet are used)
- **Evidence:** the original stack question named Tailwind as the fifth
  pillar; RFC-0026 M5+ sketched this and deferred it. Tailwind's actual
  value is (a) a constrained utility vocabulary and (b) shipping only the
  CSS that vocabulary needs — both are compile-time facts, which is where
  Vyrn puts compile-time facts. What Tailwind approximates with an LSP
  plugin and a source scanner, a generator can make a *type*.

---

## Surface

```vyrn
import { tw } from "std/tw"
import * as theme from tw("./theme.json")
// exports: type Tw, type TwClass, cls(c: Tw) -> Attr, css() -> String

fn badge(label: String, active: Bool) -> Html {
    return el("span",
        [theme.cls(if active { "px-2 rounded bg-brand-500 text-white" }
                   else { "px-2 rounded bg-gray-200" })],
        [text(label)])
}
// "bg-brnd-500" or "text-whte" = COMPILE ERROR at the literal
```

- **`theme.json`** declares: `colors` (name → shade map or single value),
  `spacing` scale, `breakpoints`, `radius`, `fontSize` — flat JSON, the
  obvious keys. The generator derives the utility vocabulary from it.
- **`type TwClass`** — the FINITE type of single class names the theme
  yields; **`type Tw`** — the regex type of space-separated non-empty
  sequences of them (`class( class)*`). String literals check at compile
  time via the existing consteval auto-validation (the `RoutePath`
  precedent); runtime strings validate at the boundary like any validated
  type.
- **`cls(c: Tw) -> Attr`** — the checked bridge into `std/html` (plain
  `cls` on a raw `String` still exists for unchecked/dynamic cases; the
  theme's `cls` is the one you *want* to reach for).
- **`css() -> String`** — the full stylesheet for the theme-derived
  vocabulary, served however the app likes (shelf: a `/theme.css` route
  returning it with the right content type).

## Vocabulary (locked, v1)

- **Theme-parameterized families:** `bg-`/`text-`/`border-` × each color
  (and shade), `p-`/`px-`/`py-`/`pt-`/`pr-`/`pb-`/`pl-` and the `m-`
  twins × the spacing scale, `gap-` × spacing, `rounded`/`rounded-` ×
  radius, `text-` × fontSize names, `w-`/`h-` × spacing.
- **Static utilities:** `flex`, `grid`, `block`, `inline`, `hidden`,
  `items-start|center|end`, `justify-start|center|end|between`,
  `flex-col`, `flex-wrap`, `font-normal|bold`, `italic`, `underline`,
  `border`, `cursor-pointer`, `select-none`, `overflow-auto|hidden`,
  `text-left|center|right`.
- **Variants:** `hover:` and `focus:` prefixes (pseudo-class emission)
  and responsive prefixes from `breakpoints` (`sm:`, `md:`, … — media
  query emission). Prefixes compose with any utility, at most one
  responsive + one state prefix per class (`md:hover:bg-brand-600`).
- **NOT in v1:** arbitrary values (`p-[3px]`), `dark:`, negative values,
  `!important`, arbitrary selectors/plugins. The vocabulary is closed —
  that closedness is what makes it a type.

## Semantics & emission (locked)

- The class language is **finite** (theme sets × fixed families ×
  bounded prefixes), so `TwClass` gets enumeration-backed LSP completion
  for free (the finite-types machinery); `Tw` is regular via
  concatenation. Malformed theme.json (unknown keys, bad shade maps,
  non-CSS-safe names — names must be `[a-z][a-z0-9-]*`) fails generation
  with a load diagnostic naming the key.
- `css()` emits deterministically ordered rules (family order above,
  theme order within a family; variants after base — responsive blocks
  last, one media query per breakpoint), so the stylesheet is
  byte-stable and cache-friendly. v1 ships the whole vocabulary's CSS
  (bounded by the theme); used-only pruning is a v2 concern and would be
  a source-scanning generator over the same data.
- The generator + `css()` are comptime-pure Vyrn (string building over
  parsed JSON — the `std/i18n` pattern; reuse its JSON reader if it is
  importable, else the generator carries its own small reader — pick
  whichever the module layout makes cleaner and say so).

## Consumers (the proof)

- **shelf** adopts it: `theme.json`, a `/theme.css` route serving
  `css()`, and its hand-written CSS replaced where utilities cover it
  (keep any truly bespoke rules in a small static file — no heroics).
  At least one compile-error demonstration in tests (a typo'd literal
  fails generation/check with the validated-type diagnostic).
- `.vyx` template `class="…"` checking against `Tw` is **deferred** —
  it needs a components↔tw coupling design (how does `components(dir)`
  learn the theme?) that should not be invented ad hoc here; noted as
  the follow-up alongside LSP embedded regions.

## Out of scope

Everything in "NOT in v1", `.vyx` template integration (deferred above),
used-only CSS pruning, non-JSON theme formats, runtime theme switching
(a second theme is a second generator import).
