# std/symbolmap

std/symbolmap (RFC-0073 M1) — the shared shape of a generated module's symbol
map: for each exported symbol, the source declaration it stands for, plus any
derived facts.

  import { symbol, strField, mapJson, symbolMapFn } from "std/symbolmap"

A BUILDER, not an emitter. A `gen fn` returns one thing — the module source —
so the map is not a second artifact beside it; it is an ordinary
`symbolMap() -> String` export baked INTO the module (`symbolMapFn` renders
the whole declaration). The generator cache already keys a module by content
hash, so a map that lives inside the module cannot go stale relative to the
code beside it, and there is no second cache entry to keep in step.

The JSON is the format third-party tools read, so it lives in one place:

  { "module": "client(./server/api)",
    "symbols": [ { "name": "pastes.list",
                   "origin": { "file": "server/api/pastes.vyrn", "line": 8,
                               "col": 11, "name": "list" },
                   "derived": { "kind": "rpc", "path": "/_/pastes/list" } } ] }

`derived` is open — it is whatever `Json` the generator puts there, so an
HTTP projection can write cache policy into it without this module learning
what cache policy is. An empty `derived` is omitted rather than written `{}`.

## Symbol

```vyrn
type Symbol = { name: String, origin: Origin, derived: Array<JsonField> }
```

One mapped symbol: the name the generated module exports, the declaration it
stands for, and the derived facts about it.

`origin` is the compiler's own `Origin` — the one `moduleInterface` hands a
generator on every `FnInfo`/`TypeInfo` — so a map records what reflection
said rather than what a generator reconstructed.

## symbol

```vyrn
fn symbol(name: String, origin: Origin, derived: Array<JsonField>) -> Symbol
```

A mapped symbol. `derived` is `[]` for a symbol with nothing derived about it.

## strField

```vyrn
fn strField(key: String, value: String) -> JsonField
```

`"key": "value"` — the shape almost every derived fact has.

## mapJson

```vyrn
fn mapJson(module: String, symbols: Array<Symbol>) -> String
```

The map document for `module` — the generator call that produced it — as
compact JSON, symbols in emission order.

## symbolMapFn

```vyrn
fn symbolMapFn(module: String, symbols: Array<Symbol>) -> String
```

The map declaration to append to a generated module — the whole of what a
generator emits for its map.

The JSON is baked as a string LITERAL through an RFC-0054 code quote:
`\{json}` sits in expression position, so the compiler's own escaping turns
the document into data. Hand-escaping it would be a second escaper free to
disagree with the lexer, over a string that already carries one layer of JSON
escaping. `gen fn` because a code quote is a generation-context construct;
every caller is a generator already.

The declaration's name carries [`mapSlug`], so the reader finds it by the
`symbolMap` PREFIX rather than by an exact name.
