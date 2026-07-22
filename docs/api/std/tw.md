# std/tw

std/tw — theme-derived utility classes as a CHECKED TYPE (RFC-0032), a library
entirely on RFC-0021 generator imports. One `gen fn`, `tw(theme)`, reads a flat
`theme.json` at compile time (sandboxed, deterministic, cached), derives a
closed utility vocabulary from it, and RETURNS a synthesized module as Vyrn
source. The compiler knows nothing about CSS or Tailwind — everything below is
comptime-pure Vyrn (string building over `bytes`/`stringFromBytes`), the same
pattern as `std/i18n` and `std/ui`.

  import { tw } from "std/tw"
  import * as theme from tw("./theme.json")
  // exports: type Tw, type TwClass, cls(c: Tw) -> Attr, css() -> String

What the generated module contains:
  - `type TwClass` — the FINITE validated string type of every SINGLE class the
    theme yields (base vocabulary × the bounded `sm:`/`md:` responsive and
    `hover:`/`focus:` state prefixes). Finite ⇒ the LSP completes it (RFC-0020
    M1); a large theme exceeds the enumeration cap and simply offers nothing.
  - `type Tw` — the regex type of space-separated non-empty sequences of single
    classes (`class( class)*`). A `String` literal argument to `cls` is checked
    against `Tw` at COMPILE TIME by the existing consteval containment machinery
    (the RoutePath precedent): `"bg-brnd-500"` is a compile error at the literal.
  - `cls(c: Tw) -> Attr` — the checked bridge into `std/html` (returns `Cls(c)`);
    `std/html`'s plain `cls` on a raw `String` still exists for dynamic cases.
  - `css() -> String` — the whole theme-derived stylesheet, baked at generation
    time into a deterministic, byte-stable string literal (served however the
    app likes; shelf: a `/theme.css` route with `text/css`).

Inspect the synthesized module with:  vyrn emit-gen <file>

theme.json (flat, the obvious keys — every leaf value is a String):
  {
    "colors":      { "brand": { "500": "#4f46e5", "600": "#4338ca" },
                     "gray": { "200": "#e5e7eb" }, "white": "#ffffff" },
    "spacing":     { "0": "0", "1": "0.25rem", "2": "0.5rem" },
    "radius":      { "DEFAULT": "0.5rem", "sm": "0.25rem", "full": "9999px" },
    "fontSize":    { "sm": "0.875rem", "base": "1rem", "lg": "1.25rem" },
    "breakpoints": { "sm": "640px", "md": "768px" }
  }
A `colors` leaf may be a single value (`"white"`) or a shade map. `radius`'s
`DEFAULT` key drives the bare `rounded`; every other radius name drives
`rounded-<name>`. Malformed theme.json (an unknown top-level key, a non-string
leaf, or a derived class name that is not `[a-z][a-z0-9-]*`) fails generation
with a load diagnostic naming the offender.

JSON-reader decision: `std/i18n` carries an equivalent object reader, but it is
module-private (not exported), so — exactly as `std/ui` carries its own
`ui*`-prefixed helpers rather than importing i18n's — this module carries its
own small `tw*`-prefixed reader. Keeping the generator self-contained beats
widening i18n's export surface for one caller.

## tw

```vyrn
fn tw(theme: String) -> String
```

`tw(theme)` — read the flat `theme.json`, derive the utility vocabulary, and
synthesize the typed module. Malformed input fails the load with a diagnostic
naming the offending key (the std convention: the offense rides a bare
top-level identifier so parsing fails immediately, attributed to this call).
