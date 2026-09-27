# std/json

std/json: the JSON value tree and its canonical writer.

  import { Json, JsonField, emit, emitPretty, jsonEq } from "std/json"
  import { parseJson } from "std/jsonread"

This module imports nothing; the reader, `std/jsonread`, imports it. So a
caller that only serializes, such as `toJson`, links no parser. `JNum` holds
raw number text and objects keep their stored field order, so `emit` is
byte-stable through a parse and generators can depend on the order.

## JsonField

```vyrn
type JsonField = { key: String, value: Json }
```

## Json

```vyrn
type Json = JNull | JBool(Bool) | JNum(String) | JStr(String) | JArr(Array<Json>) | JObj(Array<JsonField>)
```

A JSON value. `JNum` holds the raw, validated number text, never a float,
so emit -> parse -> emit is byte-stable.

## copyJson

```vyrn
fn copyJson(j: Json) -> Json
```

A `Json` that shares nothing with `j`; same as `j.copy()`.

## copyJsonArray

```vyrn
fn copyJsonArray(xs: Array<Json>) -> Array<Json>
```

`copy` over a list of values; `xs.copy()` is refused for this element type.

## copyJsonFields

```vyrn
fn copyJsonFields(fs: Array<JsonField>) -> Array<JsonField>
```

`copy` over a list of object fields, for the same reason.

## sortKeys

```vyrn
fn sortKeys(j: Json) -> Json
```

A copy of the tree with every object's fields in byte order, recursively.
Use it for one emission (a diff, content-addressed output); the stored
order, which mirrors a `Map`'s insertion order, never moves.

## emit

```vyrn
fn emit(j: Json) -> String
```

Emit `j` as compact JSON, field order as stored.

The writers, `copyJson` and `jsonEq` recurse at about two frames a level,
so a tree nested past about 450 levels traps at the engine's call cap.
`parseJson` refuses input past 128 levels, so only a tree a program builds
that deep can reach the cap; bounding it is the program's job.

## emitPretty

```vyrn
fn emitPretty(j: Json, indent: Int64) -> String
```

Emit `j` as indented JSON, `indent` spaces per level, field order as
stored. Empty arrays and objects stay compact (`[]`, `{}`).

## jsonEq

```vyrn
fn jsonEq(a: Json, b: Json) -> Bool
```

Deep structural equality, as equality of canonical emits: `emit` is
injective over `Json`, because kinds have distinct delimiters, field order
is kept and numbers keep their raw text.
