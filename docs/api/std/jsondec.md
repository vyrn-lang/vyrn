# std/jsondec

std/jsondec: the untyped half of `fromJson`, and the generator of the
typed half.

`jsonDecoders` writes the typed half per target type; what it writes
calls this module for kind names, `Issue`s, paths, tree accessors and
scalar decoders. Decoding accumulates: a decoder records its
failure and returns, and a composite constructs only when every part
succeeded. So every decoder returns an `Array<T>` of zero or one element;
empty means the issue is already recorded. `Option<T>` cannot serve: with
`T = Option<U>` it has no wire form. Every message is the canonical wording,
byte for byte.

## kindName

```vyrn
fn kindName(v: Json) -> String
```

The JSON kind name used in `expected <X>, found <kind>`.

## isNull

```vyrn
fn isNull(v: Json) -> Bool
```

True for `null`, the wire form of an absent `Option`.

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

## keyOf

```vyrn
fn keyOf(v: Json) -> String
```

The single key of a one-member object (a payload variant's wire form), or
`""` for anything else. Reads the fields in place: `fieldsOf` would copy
the whole object.

## valOf

```vyrn
fn valOf(v: Json) -> Json
```

The single value of a one-member object, or `JNull`. Reads in place, as
`keyOf` does.

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

## readDoc

```vyrn
fn readDoc(src: String, iss: modify Array<Issue>) -> Array<Json>
```

Parse `src` into a one-element array, or record one `json.parse` Issue and
return `[]`.

## dStr

```vyrn
fn dStr(v: Json, path: String, iss: modify Array<Issue>) -> Array<String>
```

## dBool

```vyrn
fn dBool(v: Json, path: String, iss: modify Array<Issue>) -> Array<Bool>
```

## dInt64

```vyrn
fn dInt64(v: Json, path: String, iss: modify Array<Issue>) -> Array<Int64>
```

A JSON integer-syntax number as an exact `Int64`. A fraction, an exponent or
a magnitude outside `Int64` is `expected integer`, never a rounded value.

## dIntKey

```vyrn
fn dIntKey(key: String, path: String, iss: modify Array<Issue>) -> Array<Int64>
```

An object key read as an `Int64`, the wire form of `Map<Int64, V>`. Only
the canonical text (`n.toString()`) passes: `"007"` and `"+7"` would
otherwise collapse into the key `"7"` with no Issue.

## dIntRange

```vyrn
fn dIntRange(v: Json, path: String, iss: modify Array<Issue>, lo: Int64, hi: Int64) -> Array<Int64>
```

`dInt64` restricted to `[lo, hi]`, for the sized signed integers.

## dUIntMax

```vyrn
fn dUIntMax(v: Json, path: String, iss: modify Array<Issue>, hi: UInt64) -> Array<UInt64>
```

A JSON integer-syntax number as an exact `UInt64` in `[0, hi]`.
`parseInt64` cannot serve: it has no room above `Int64.max`.

## dFloat64

```vyrn
fn dFloat64(v: Json, path: String, iss: modify Array<Issue>) -> Array<Float64>
```

A JSON number as a `Float64`, correctly rounded (`std/num`). Integer syntax is
accepted: `1` decodes into a `Float64` target as `1.0`.

## dFloat32

```vyrn
fn dFloat32(v: Json, path: String, iss: modify Array<Issue>) -> Array<Float32>
```

A JSON number as a `Float32`, rounded once: decimal -> `Float64` ->
`Float32` rounds twice and is wrong near a `Float32` tie.

## jsonDecoders

```vyrn
fn jsonDecoders(t: TypeArg) -> String
```

Writes the decoders `fromJson` calls: for each root, the entry that reads a
document into `Validation<T>`, and for each node, the function that decodes
a `Json` tree into zero or one value. Generated source cannot spell reserved
names, so it writes `VyrnRt_` for `std/json`'s, `VyrnRd_` for this module's
and `VyrnWp_` for a `where` type's predicate.
