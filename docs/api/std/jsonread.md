# std/jsonread

std/jsonread: the strict JSON reader over the `Json` tree of `std/json`.

  import { parseJson } from "std/jsonread"
  import { Json, JsonField, emit } from "std/json"

The reader imports the writer's tree, never the reverse, so a caller that
only serializes links `std/json` alone. The name is `parseJson` because
`parse` is a reserved builtin. Commas are required, trailing commas and
duplicate keys are refused, every escape is decoded (a lone surrogate is an
error), and object fields keep source order. Offsets are bytes; every error
starts with `line N, col M:`.

## parseJson

```vyrn
fn parseJson(src: String) -> Result<Json, String>
```

Parses a whole JSON document, or returns a `line N, col M: <reason>` error.
Numbers are validated and stored as raw text; nesting past [`maxDepth`] is
an `Err`.
