# std/jsonread

The strict JSON reader: `parseJson`, and the cursor `fromJson` reads with.

  import { parseJson } from "std/jsonread"
  import { Json, JsonField, emit } from "std/json"

The reader imports the writer's tree, never the reverse, so a caller that
only serializes links `std/json` alone. The name is `parseJson` because
`parse` is a reserved builtin. Commas are required, trailing commas and
duplicate keys are refused, every escape is decoded (a lone surrogate is an
error), and object fields keep source order. Offsets are bytes; every error
starts with `line N, col M:`.

The grammar lives in the `JsonReader` functions: `parseJson` builds a tree
with them, and a `fromJson` decoder writes straight into its record. Each
value reader expects the cursor on the value's first byte and leaves it on
the byte after the value. The first refusal is the document's error; it
moves the cursor to the end, so every later reader sees the end of input,
refuses nothing more, and returns.

## jsonReader

```vyrn
fn jsonReader(s: String) -> JsonReader
```

A reader over `s`, its cursor on the first byte that is not whitespace.

## jsonRefused

```vyrn
fn jsonRefused(p: JsonReader) -> Bool
```

Whether the document is refused; `p.err` holds the message.

## finishJson

```vyrn
fn finishJson(p: modify JsonReader) -> Unit
```

Refuses content after the document's one value.

## jsonKindAt

```vyrn
fn jsonKindAt(p: JsonReader) -> Int64
```

The kind of the value at the cursor, named by a byte that starts it: `{`,
`[`, `"`, `t` for a boolean, `n` for `null` and `0` for a number. `-1`
where no value can start, which a read then refuses.

## jsonKindName

```vyrn
fn jsonKindName(k: Int64) -> String
```

A `jsonKindAt` kind as `expected <X>, found <kind>` words it; `""` for `-1`.

## readJsonString

```vyrn
fn readJsonString(p: modify JsonReader) -> String
```

Reads a JSON string, the cursor on the opening `"`, and decodes every
escape to UTF-8. Refused, it answers `""`.

## readJsonNumber

```vyrn
fn readJsonNumber(p: modify JsonReader) -> String
```

Reads a JSON number, the cursor on `-` or a digit, and returns its raw text.
Grammar: `-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?`. Refused, it
answers `""`.

## readJsonKeyword

```vyrn
fn readJsonKeyword(p: modify JsonReader) -> Bool
```

Reads the keyword `true`, `false` or `null`, the cursor on its first byte,
and answers whether it is `true`.

## enterJsonObject

```vyrn
fn enterJsonObject(p: modify JsonReader) -> Bool
```

`enter` for the object at the cursor.

## enterJsonArray

```vyrn
fn enterJsonArray(p: modify JsonReader) -> Bool
```

`enter` for the array at the cursor.

## readJsonKey

```vyrn
fn readJsonKey(p: modify JsonReader) -> String
```

Reads the key of an object member, the cursor on it. Refused, it answers
`""`. The caller checks the key for a duplicate before `readJsonColon`, so
a duplicate is refused just past the key.

## readJsonKeyIf

```vyrn
fn readJsonKeyIf(p: modify JsonReader, name: String) -> Bool
```

Reads the key at the cursor and answers `true` if it is `name` spelled with
no escape. Otherwise it moves nothing and answers `false`, and `readJsonKey`
reads the key. It allocates nothing.

## readJsonColon

```vyrn
fn readJsonColon(p: modify JsonReader) -> Unit
```

Reads the `:` after an object key, the cursor then on the value.

## nextJsonMember

```vyrn
fn nextJsonMember(p: modify JsonReader) -> Bool
```

Reads what follows an object member's value and answers whether another
member follows, the cursor on its key.

## nextJsonItem

```vyrn
fn nextJsonItem(p: modify JsonReader) -> Bool
```

Reads what follows an array element and answers whether another element
follows, the cursor on it.

## JsonKeySet

```vyrn
type JsonKeySet = { keys: Array<String>, slots: Array<Int64>, mask: Int64 }
```

The keys one object has shown: linear probing over FNV-1a-64, capacity a
power of two (`mask = cap - 1`), grown at 70% load. A scan per member would
make an N-key object O(N^2), which one wide untrusted body could exploit.
Empty, it holds no slots, so a set no key reaches allocates nothing.

## jsonKeySet

```vyrn
fn jsonKeySet() -> JsonKeySet
```

## admitJsonKey

```vyrn
fn admitJsonKey(p: modify JsonReader, seen: modify JsonKeySet, key: String) -> Bool
```

Records `key` in `seen` and answers `true`, or refuses it as a duplicate
and answers `false`.

## refuseJsonDuplicate

```vyrn
fn refuseJsonDuplicate(p: modify JsonReader, key: String) -> Unit
```

Refuses `key` as a repeat of a key its object already holds.

## skipJsonValue

```vyrn
fn skipJsonValue(p: modify JsonReader) -> Unit
```

Reads and drops one value, refusing what `parseJson` refuses.

## parseJson

```vyrn
fn parseJson(src: String) -> Result<Json, String>
```

Parses a whole JSON document, or returns a `line N, col M: <reason>` error.
Numbers are validated and stored as raw text; nesting past `maxDepth` is
an `Err`.
