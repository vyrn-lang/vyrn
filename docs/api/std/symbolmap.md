# std/symbolmap

std/symbolmap: the symbol map of a generated module. For each
exported symbol, the source declaration it stands for and any derived facts.

  import { symbol, strField, mapJson, symbolMapFn } from "std/symbolmap"

The map is a `symbolMap<Slug>() -> String` export baked into the module, so
the generator cache, keyed by content, cannot let it go stale. Third-party
tools read this JSON:

  { "module": "client(./server/api)",
    "symbols": [ { "name": "pastes.list",
                   "origin": { "file": "server/api/pastes.vyrn", "line": 8,
                               "col": 11, "name": "list" },
                   "derived": { "kind": "rpc", "path": "/_/pastes/list" } } ] }

`derived` is whatever `Json` the generator puts there; an empty one is
omitted.

## Symbol

```vyrn
type Symbol = { name: String, origin: Origin, derived: Array<JsonField> }
```

One mapped symbol. `origin` is the `Origin` reflection handed the generator,
not one the generator reconstructed.

## symbol

```vyrn
fn symbol(name: String, origin: Origin, derived: consume Array<JsonField>) -> Symbol
```

`derived` is `[]` for a symbol with nothing derived about it.

## strField

```vyrn
fn strField(key: String, value: String) -> JsonField
```

## mapJson

```vyrn
fn mapJson(module: String, symbols: Array<Symbol>) -> String
```

The map document for the generator call `module`, as compact JSON, symbols
in emission order.

## symbolMapFn

```vyrn
fn symbolMapFn(module: String, symbols: Array<Symbol>) -> String
```

The map declaration to append to a generated module.

The JSON goes through a code quote: `\{json}` sits in expression position,
so the compiler's own escaping makes it a string literal, with no second
escaper to disagree with the lexer. The reader finds the declaration by the
`symbolMap` prefix, since `mapSlug` names it.
