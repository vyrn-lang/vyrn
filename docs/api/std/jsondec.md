# std/jsondec

The untyped half of `fromJson`, and the generator of the
typed half.

`jsonDecoders` writes the typed half per target type. What it writes reads
the source once through a `JsonReader` and decodes each value into its slot;
it builds no `Json` tree. It calls this module for the reader, `Issue`s,
paths and scalar decoders. Decoding accumulates: a decoder records its
failure and returns, and a composite constructs only when every part
succeeded. So every decoder returns an `Array<T>` of zero or one element;
empty means the issue is already recorded. `Option<T>` cannot serve: with
`T = Option<U>` it has no wire form. Every message is the canonical wording,
byte for byte. A refused document yields its `json.parse` Issue alone, and a
record's Issues follow its members' declaration order, whatever the
document's key order.

The tree accessors (`fieldsOf`, `fieldAt` and the rest) serve programs that
read a `Json` from `parseJson`.

## fieldsOf

```vyrn
fn fieldsOf(v: Json) -> Array<JsonField>
```

An object's fields in document order, or `[]` for anything else.

## itemsOf

```vyrn
fn itemsOf(v: Json) -> Array<Json>
```

An array's items, or `[]` for anything else.

## numText

```vyrn
fn numText(v: Json) -> String
```

A number's raw source text, or `""` for anything else.

## hasField

```vyrn
fn hasField(fs: Array<JsonField>, key: String) -> Bool
```

Whether an object carries `key` at all (a present `null` is still present).

## fieldAt

```vyrn
fn fieldAt(fs: Array<JsonField>, key: String) -> Json
```

The value of `key`, or `JNull` when absent.

## elemAt

```vyrn
fn elemAt(items: Array<Json>, i: Int64) -> Json
```

Element `i`, or `JNull` when the index is past the end.

## tagOf

```vyrn
fn tagOf(v: Json) -> String
```

A bare string's content (a nullary enum variant's wire form), or `""`. No
variant can be named `""`, so the sentinel cannot collide with a real tag.

## pushType

```vyrn
fn pushType(iss: modify Array<Issue>, path: String, expected: String, found: String) -> Unit
```

`json.type`: `expected <what>, found <kind>`.

## pushMissing

```vyrn
fn pushMissing(iss: modify Array<Issue>, path: String, field: String) -> Unit
```

``json.missing``: ``missing required field `name` ``.

## pushValidate

```vyrn
fn pushValidate(iss: modify Array<Issue>, path: String, message: String) -> Unit
```

`validate`: a refined type's `where` clause did not hold. The caller passes
the compiler's validation message, so the trap and this path word it the
same.

## fieldPath

```vyrn
fn fieldPath(parent: String, field: String) -> String
```

A dotted path extended by a record field (or an enum tag).

## indexPath

```vyrn
fn indexPath(parent: String, i: Int64) -> String
```

A path extended by an array index.

## pathAt

```vyrn
fn pathAt(path: String, seg: String, idx: Int64) -> String
```

The path of a value a decoder reads: `path` itself when `idx` is `-2`,
`path[idx]` for an element, and `path.seg` for a member when `idx` is `-1`.
A decoder builds it only for an Issue or for its own members, so a value
that decodes builds no path.

## openDoc

```vyrn
fn openDoc(src: String) -> JsonReader
```

A reader over `src`.

## closeDoc

```vyrn
fn closeDoc(p: modify JsonReader) -> Array<Issue>
```

Ends a decode: `[]`, or the one `json.parse` Issue of a refused document.

## kindAt

```vyrn
fn kindAt(p: JsonReader) -> Int64
```

The kind of the value at the cursor, as `std/jsonread`'s `kindAt` names it.

## kindName

```vyrn
fn kindName(k: Int64) -> String
```

## expect

```vyrn
fn expect(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>, kind: Int64, what: String) -> Bool
```

Whether the value at the cursor is of `kind`, a `kindAt` kind. If it is
not, records `expected <what>, found <kind>` at the value's path (see
`pathAt`) and reads the value past.

## readNull

```vyrn
fn readNull(p: modify JsonReader) -> Bool
```

Reads a `null` at the cursor and answers whether there was one: the wire
form of an absent `Option`.

## readString

```vyrn
fn readString(p: modify JsonReader) -> String
```

## skipValue

```vyrn
fn skipValue(p: modify JsonReader) -> Unit
```

## enterObject

```vyrn
fn enterObject(p: modify JsonReader) -> Bool
```

## enterArray

```vyrn
fn enterArray(p: modify JsonReader) -> Bool
```

## readKey

```vyrn
fn readKey(p: modify JsonReader) -> String
```

## keyIs

```vyrn
fn keyIs(p: modify JsonReader, name: String) -> Bool
```

## nextMember

```vyrn
fn nextMember(p: modify JsonReader) -> Bool
```

## nextItem

```vyrn
fn nextItem(p: modify JsonReader) -> Bool
```

## keySet

```vyrn
fn keySet() -> JsonKeySet
```

## member

```vyrn
fn member(p: modify JsonReader, seen: Bool, key: String) -> Unit
```

Reads past the `:` after a record member's `key`, or refuses the key as a
duplicate when the member was `seen`.

## mapMember

```vyrn
fn mapMember(p: modify JsonReader, keys: modify JsonKeySet, key: String) -> Bool
```

Records `key` in `keys` and reads past the `:`, answering `true`; or
refuses the key as a duplicate and answers `false`.

## skipMember

```vyrn
fn skipMember(p: modify JsonReader, keys: modify JsonKeySet, key: String) -> Unit
```

Reads a member no field claims: an unknown field is ignored, but its key
may not repeat and its value must be JSON.

## takeIssues

```vyrn
fn takeIssues(iss: modify Array<Issue>, base: Int64) -> Array<Issue>
```

Moves `iss[base..]` out, in order: a record puts its members' Issues back
in declaration order with `putIssues`.

## putIssues

```vyrn
fn putIssues(iss: modify Array<Issue>, late: Array<Issue>, a: Int64, b: Int64) -> Unit
```

Appends `late[a..b]`, end excluded, to `iss`.

## dStr

```vyrn
fn dStr(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>) -> Array<String>
```

## dBool

```vyrn
fn dBool(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>) -> Array<Bool>
```

## dInt64

```vyrn
fn dInt64(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>) -> Array<Int64>
```

A JSON integer-syntax number as an exact `Int64`. A fraction, an exponent or
a magnitude outside `Int64` is `expected integer`, never a rounded value.

## dIntKey

```vyrn
fn dIntKey(key: String, path: String, iss: modify Array<Issue>) -> Array<Int64>
```

An object key read as an `Int64`, the wire form of `Map<Int64, V>`, in the
map at `path`. Only the canonical text (`n.toString()`) passes: `"007"`
and `"+7"` would otherwise collapse into the key `"7"` with no Issue.

## dIntRange

```vyrn
fn dIntRange(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>, lo: Int64, hi: Int64) -> Array<Int64>
```

`dInt64` restricted to `[lo, hi]`, for the sized signed integers.

## dUIntMax

```vyrn
fn dUIntMax(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>, hi: UInt64) -> Array<UInt64>
```

A JSON integer-syntax number as an exact `UInt64` in `[0, hi]`.
`parseInt64` cannot serve: it has no room above `Int64.max`.

## dFloat64

```vyrn
fn dFloat64(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>) -> Array<Float64>
```

A JSON number as a `Float64`, correctly rounded (`std/num`). Integer syntax is
accepted: `1` decodes into a `Float64` target as `1.0`.

## dFloat32

```vyrn
fn dFloat32(p: modify JsonReader, path: String, seg: String, idx: Int64, iss: modify Array<Issue>) -> Array<Float32>
```

A JSON number as a `Float32`, rounded once: decimal -> `Float64` ->
`Float32` rounds twice and is wrong near a `Float32` tie.

## jsonDecoders

```vyrn
fn jsonDecoders(t: TypeArg) -> String
```

Writes the decoders `fromJson` calls: for each root, the entry that reads a
document into `Validation<T>`, and for each node, the function that decodes
the value at a reader's cursor into zero or one value. Generated source
cannot spell reserved names, so it writes `VyrnRt_` for `std/json`'s,
`VyrnRd_` for this module's and `VyrnWp_` for a `where` type's predicate.
