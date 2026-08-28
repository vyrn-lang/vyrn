# std/json5

std/json5 — a JSON5 reader producing the SAME `Json` tree `std/json` emits.

  import { parseJson5 } from "std/json5"

One tree, two readers: a strict document goes through `std/jsonread`, a
hand-written config through this. There is deliberately no JSON5 WRITER:
numbers are normalized to their strict spelling AT THE PARSE (`0x1A` → `26`,
`.5` → `0.5`, `5.` → `5`, `+5` → `5`), because `JNum` holds raw text that
`emit` re-emits verbatim — so whatever this reader builds, the writer still
writes valid strict JSON. JSON5 is a reading convenience, never a second
wire form (RFC-0117 §5).

What the grammar adds over strict JSON, all honored here: `//` and `/* */`
comments, trailing commas in arrays and objects, unquoted object keys,
single-quoted strings, the extra escapes (`\'`, `\v`, `\0`, `\xHH`, an
escaped literal character) and line continuations, hex integers, leading
`+`, and leading or trailing decimal points. The extended whitespace set
(vertical tab, form feed, NBSP, U+2028/U+2029, BOM) is skipped anywhere
whitespace may appear.

Two named refusals, both about the tree rather than the grammar:
`Infinity` and `NaN` have no strict-JSON spelling for `emit` to write, so
they are errors here; and a hex literal past `Int64`'s ceiling has no exact
decimal text to normalize to. One deviation: an unquoted key is an ASCII
identifier (`$`, `_`, letters, digits) — Unicode identifier keys and `\u`
escapes in identifiers are not recognized; quote such a key.

## parseJson5

```vyrn
fn parseJson5(src: String) -> Result<Json, String>
```

Parse one JSON5 document: a single value, with gaps (whitespace and
comments) allowed around it and nothing after it.
