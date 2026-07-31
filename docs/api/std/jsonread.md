# std/jsonread

std/jsonread (RFC-0059, split out by RFC-0078 M2a) — the STRICT JSON reader,
over the `Json` tree `std/json` declares.

  import { parseJson } from "std/jsonread"
  import { Json, JsonField, emit } from "std/json"

The split is one-directional by construction: the reader imports the writer's
tree, never the reverse. That is what lets `std/json` — the type plus the
canonical writer — be linked on its own by a caller that only serializes,
which is what RFC-0078's `toJson` needs and what the direct wasm backend can
compile today (the reader still wants `?` and `if let`, RFC-0077's own rows).

NOTE: the reader is `parseJson`, not `parse` — `parse` is a reserved language
builtin (`parse(String) -> Option<Int64>`), so the RFC's locked `parse` name is
unavailable to a user module.

A `String` is UTF-8 bytes; all offsets are BYTE offsets (like `std/strings`).
`parseJson` is strict where the hand-rollers it replaced were lenient: commas
are REQUIRED between members, trailing commas are REJECTED, duplicate object
keys are REJECTED (naming the key), and the full escape set — including
`\uXXXX` with surrogate pairs decoded to UTF-8 — is honored (a lone surrogate
is an error). Every error carries a `line N, col M:` prefix. Object field order
is preserved in source order, so deterministic generators can depend on it.

## parseJson

```vyrn
fn parseJson(src: String) -> Result<Json, String>
```

Parse a whole JSON document into a `Json` tree, or a `line N, col M: <reason>`
error. STRICT: commas required, trailing commas rejected, duplicate keys
rejected, full escapes (incl. `\uXXXX` surrogate pairs), numbers validated and
stored raw, object field order preserved.
