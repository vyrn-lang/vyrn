# std/json

std/json (RFC-0059) — the shared JSON value tree and its canonical writer,
written in plain Vyrn on `bytes`/`stringFromBytes`. Being ordinary
comptime-pure Vyrn, it is importable anywhere — the generators (`std/tw`,
`std/i18n`, `std/openapi`) are its first consumers, replacing three JSON
hand-rollers that were each lenient in a different way.

  import { Json, JsonField, emit, emitPretty, jsonEq } from "std/json"
  import { parseJson } from "std/jsonread"          // the reader

RFC-0078 M2a moved the reader to `std/jsonread`, which imports this module —
one direction only. This file therefore imports NOTHING, so a caller that only
serializes links only the writer. That matters because `toJson` became
exactly such a caller in RFC-0078 M2b: it links the tree and `emit` without
dragging in a parser, and the direct wasm backend compiles this half today
(the reader still wants `?` and `if let`, RFC-0077's own rows).

A `String` is UTF-8 bytes; all offsets are BYTE offsets (like `std/strings`).
`JNum` holds raw validated number text, so `emit` is byte-stable through a
parse and object field order is whatever the tree stores — deterministic
generators depend on both.

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

## copyJson

```vyrn
fn copyJson(j: Json) -> Json
```

A `Json` that shares nothing with `j` — the name this walk had before it was
a `Copy` impl. `j.copy()` says the same thing and is the one to write.

## copyJsonArray

```vyrn
fn copyJsonArray(xs: Array<Json>) -> Array<Json>
```

`copy` over a list of values. Exported because `xs.copy()` cannot be written
for this element type — see the impl above.

## copyJsonFields

```vyrn
fn copyJsonFields(fs: Array<JsonField>) -> Array<JsonField>
```

`copy` over a list of object fields, for `xs.copy()`'s reason.

## emit

```vyrn
fn emit(j: Json) -> String
```

Emit `j` as compact JSON (no insignificant whitespace), field order as stored.

**The writers recurse per level, and they have no error channel to refuse a
tree with.** `emit`, `emitPretty`, `copyJson` and `jsonEq` all walk `Json`
structurally at about two frames a level, so a value nested past roughly 450
reaches the engine's 1000-frame call cap and traps. Where the depth comes from
UNTRUSTED bytes that is a defect, and it is fixed at the reader:
`std/jsonread`'s `maxDepth` refuses a document past 128 levels with an `Err`,
so nothing parsed can reach the ceiling here, and the round trip
`parse → emit` is covered end to end. What is left is a tree a program BUILT
that deep on purpose, which is the program's own bound to keep — a `String`
return has nowhere to say otherwise, and adding a `Result` to the serializer
every caller uses would pay for a case no input can cause.

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
