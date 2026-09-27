# std/json5

std/json5: a JSON5 reader that builds the `Json` tree of `std/json`.

  import { parseJson5 } from "std/json5"

The reader writes every number in its strict spelling (`0x1A` -> `26`,
`.5` -> `0.5`, `+5` -> `5`), so `emit` writes strict JSON for any tree it
builds; there is no JSON5 writer. It refuses
`Infinity`, `NaN` and a hex literal past `Int64`, because none has an exact
strict spelling. An unquoted key is an ASCII identifier (`$`, `_`, letters,
digits); quote any other key.

## parseJson5

```vyrn
fn parseJson5(src: String) -> Result<Json, String>
```

Parse one JSON5 document: a single value, with gaps (whitespace and
comments) allowed around it and nothing after it.
