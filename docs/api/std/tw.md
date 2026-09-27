# std/tw

Theme-derived utility classes as a checked type. `tw(theme)` reads
a flat `theme.json` at generation time and returns a module with `TwClass`
(one class; finite, so the LSP completes it), `Tw` (space-separated
classes), `cls(c: Tw) -> Attr`, and `css() -> String`, the baked stylesheet.
A literal passed to `cls` is checked against `Tw` at compile time.

  import { tw } from "std/tw"
  import * as theme from tw("./theme.json")

theme.json:
  {
    "colors":      { "brand": { "500": "#4f46e5" }, "white": "#ffffff" },
    "spacing":     { "0": "0", "1": "0.25rem" },
    "radius":      { "DEFAULT": "0.5rem", "full": "9999px" },
    "fontSize":    { "sm": "0.875rem" },
    "breakpoints": { "sm": "640px", "md": "768px" },
    "safelist":    ["book-card"]
  }
`radius.DEFAULT` drives the bare `rounded`. Safelisted names join the type
but get no CSS rule. An unknown top-level key, a non-string leaf, an unsafe
class name or an unsafe CSS value fails generation, naming the offender.

## tw

```vyrn
fn tw(theme: String) -> String
```

Reads the flat `theme.json` and returns the typed module. A malformed theme
fails the load with a report naming the offending key.
