# std/jsondec

std/jsondec (RFC-0078 M3) — the untyped half of `fromJson`, in Vyrn.

`fromJson(T, s)` is two halves, exactly as `toJson` is (RFC-0078 M2b):

  read(s)          -> a `Json` tree                 [std/jsonread — not typed]
  decode(tree, T)  -> a value of T, plus `Issue`s   [needs the compiler]

The second half is generated per target type by `vyrn-frontend`'s `jsondec`
and calls into this module for everything that is NOT type-directed: the kind
names, the `Issue` vocabulary, the path arithmetic, the tree accessors and the
scalar decoders. So the compiler part is a walk over a declaration and nothing
else — no parser, no number conversion, no message assembly, on any engine.

# The 0-or-1 array is the decode result convention

RFC-0018 decode ACCUMULATES: it does not stop at the first problem, so a
decoder must report a failure and keep walking, and a composite must construct
only once every part succeeded (a refined type cannot hold a value that failed
its own predicate — there is no zeroed slot a Vyrn program can spell).

RFC-0078 M3 predicted `Option<T>` for that. It cannot be: a bare `Option<U>`
IS a decode target (`Array<Option<Int64>>` decodes today), and `Option<Option<U>>`
is rejected by the checker — "nested Option/Result is not supported in v0.1".
So every decoder returns `Array<T>` with **zero or one** element: empty means
"no value, and the issue is already recorded". One convention for every T,
including `T = Option<U>`.

# Wording

Every message here is RFC-0018's, byte for byte. A parse error's is
`std/jsonread`'s `line N, col M: <reason>` — RFC-0078's strictness ruling
replaced the C reader's 0-based `at position N`.

## kindName

```vyrn
fn kindName(v: Json) -> String
```

The JSON kind name used in `expected <X>, found <kind>`.

## isNull

```vyrn
fn isNull(v: Json) -> Bool
```

True for `null` — the wire form of an absent `Option`.

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

The value of `key`, or `JNull` when absent. First occurrence wins — which
`std/jsonread` makes unreachable, since it rejects a duplicate key outright.

## elemAt

```vyrn
fn elemAt(items: Array<Json>, i: Int64) -> Json
```

Element `i`, or `JNull` when the index is past the end. Tuple payloads
never reach this past their own end — the generated decoder checks the
wire arity first, so a short payload is refused before any member decodes.

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
`""` — including for an object with zero or two or more members, which is the
"exactly one wire form per value" rule.
It reads the fields in place rather than through `fieldsOf`, which owes its
caller a copy of the whole object (RFC-0092). One key is what leaves here, so
one key is what is copied.

## valOf

```vyrn
fn valOf(v: Json) -> Json
```

The single value of a one-member object, or `JNull`. Reads in place, for
`keyOf`'s reason.

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

`validate`: a refined type's `where` clause did not hold. The message is the
compiler's own canonical validation wording, passed in rather than built here
so the trap path and this path cannot word it differently.

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

Parse `src` into a one-element array, or record the single `json.parse` Issue
and return `[]`. The wording is `std/jsonread`'s `line N, col M: <reason>`.

## dStr

```vyrn
fn dStr(v: Json, path: String, iss: modify Array<Issue>) -> Array<String>
```

A JSON string as a `String`.

## dBool

```vyrn
fn dBool(v: Json, path: String, iss: modify Array<Issue>) -> Array<Bool>
```

A JSON boolean as a `Bool`.

## dInt64

```vyrn
fn dInt64(v: Json, path: String, iss: modify Array<Issue>) -> Array<Int64>
```

A JSON integer-syntax number as an exact `Int64`. A fractional or exponent
form, or a magnitude outside `Int64`, is `expected integer` — never a value
rounded through a `Float64`.

## dIntKey

```vyrn
fn dIntKey(key: String, path: String, iss: modify Array<Issue>) -> Array<Int64>
```

An object KEY that must spell an `Int64` — the read half of RFC-0117 M3's
wire form for `Map<Int64, V>`: the object's keys are the decimal texts the
encoder writes with `toString`. CANONICAL text only (`n.toString()` must
give the key back), so `"007"` and `"+7"` are refused rather than silently
aliasing the key `"7"` — a leniency here would let two spellings of one
key collapse into one entry with no Issue saying so.

## dIntRange

```vyrn
fn dIntRange(v: Json, path: String, iss: modify Array<Issue>, lo: Int64, hi: Int64) -> Array<Int64>
```

The same, restricted to `[lo, hi]` — the sized signed integers.

## dUIntMax

```vyrn
fn dUIntMax(v: Json, path: String, iss: modify Array<Issue>, hi: UInt64) -> Array<UInt64>
```

A JSON integer-syntax number as an exact `UInt64`, restricted to `[0, hi]` —
the case the `parse` builtin cannot serve at all, since its `Option<Int64>`
has no room above `Int64.max` (RFC-0078 M4a).

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

A JSON number as a `Float32`, rounded to single precision DIRECTLY rather
than through a `Float64` — decimal -> `Float64` -> `Float32` rounds twice and
is wrong near a `Float32` tie (RFC-0078 M4a).
