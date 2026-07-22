# std/json

std/json (RFC-0059) — one shared JSON value tree with a STRICT reader and a
canonical writer, written in plain Vyrn on `bytes`/`stringFromBytes`/`slice`.
Being ordinary comptime-pure Vyrn, it is importable anywhere — the generators
(`std/tw`, `std/i18n`, `std/openapi`) are its first consumers, replacing three
hand-rolled JSON hand-rollers that were each lenient in a different way.

  import { Json, JsonField, parseJson, emit, emitPretty, jsonEq } from "std/json"

NOTE: the reader is `parseJson`, not `parse` — `parse` is a reserved language
builtin (`parse(String) -> Option<Int64>`), so the RFC's locked `parse` name is
unavailable to a user module. This is the sole naming deviation; `emit` and
`emitPretty` land as locked.

A `String` is UTF-8 bytes; all offsets are BYTE offsets (like `std/strings`).
`parse` is strict where the hand-rollers were lenient: commas are REQUIRED
between members, trailing commas are REJECTED, duplicate object keys are
REJECTED (naming the key), and the full escape set — including `\uXXXX` with
surrogate pairs decoded to UTF-8 — is honored (a lone surrogate is an error).
Every error carries a `line N, col M:` prefix. Object field order is preserved
in source order, so deterministic generators can depend on it.

## JsonField

```vyrn
type JsonField = { key: String, value: Json }
```

One `key: value` member of a JSON object.

## Json

```vyrn
type Json = JNull | JBool(Bool) | JNum(String) | JStr(String) | JArr(Array<Json>) | JObj(Array<JsonField>)
```

A JSON value. `JNum` carries the RAW, validated number text (never a float):
generators compare and re-emit numbers, nobody needs float semantics at
comptime, and raw text makes emit → parse → emit byte-stable.

## parseJson

```vyrn
fn parseJson(src: String) -> Result<Json, String>
```

Parse a whole JSON document into a `Json` tree, or a `line N, col M: <reason>`
error. STRICT: commas required, trailing commas rejected, duplicate keys
rejected, full escapes (incl. `\uXXXX` surrogate pairs), numbers validated and
stored raw, object field order preserved.

## emit

```vyrn
fn emit(j: Json) -> String
```

Emit `j` as compact JSON (no insignificant whitespace), field order as stored.

## emitPretty

```vyrn
fn emitPretty(j: Json, indent: Int64) -> String
```

Emit `j` as indented JSON, `indent` spaces per nesting level, field order as
stored. Empty arrays/objects stay compact (`[]`/`{}`).

## jsonEq

```vyrn
fn jsonEq(a: Json, b: Json) -> Bool
```

Deep structural equality of two `Json` trees, defined as CANONICAL-EMIT
equality: `emit` is injective over `Json` (distinct kinds carry distinct
delimiters, object field order is preserved, numbers keep their raw text), so
two trees serialize to the same bytes iff they are structurally identical.
This is exact structural equality and needs no wildcard match — the language
has none, and `==` is scalar-only, so a hand-written recursive comparator
would need every variant pair spelled out; the canonical form is the tool
consumers (and the round-trip test) use to compare trees.
