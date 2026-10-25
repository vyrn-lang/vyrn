# std/cli

The command line is a record type, and `cli` writes its parser and help.

  import { cli } from "std/cli"
  import { parseServe, helpServe } from cli("./serve")

`cli` is a `gen fn` that reflects a module of type declarations and returns a
module exporting, for every exported record type, `parse<Name>(argv) ->
Validation<Name>` and `help<Name>() -> String`. The compiler knows nothing
about command lines. The mapping is a rule over the declaration, never an
attribute:

| field type | surface | missing |
|---|---|---|
| `Bool` | `--verbose` | `false` |
| `Option<T>` | `--host <value>` | `None` |
| bare `T` | `--port <value>` | an `Issue` at path `port` |
| `Array<String>` | the positionals, at most one field | empty |

- The long name is the field name in kebab-case: `dryRun` is `--dry-run`.
- The short name is the first byte of the field name, taken in declaration
  order and skipped on collision. `-h` is reserved for `--help`.
- An unknown option is an `Issue`: unlike `std/args`, a spec exists.
- A value is validated by construction: `--port 99999` reaches
  `Port?(99999)`, and its `None` becomes an `Issue` worded from that type's
  `Schema`, so the parser and the program run one check.
- An option's help text is the `///` above its named type
  (`TypeInfo.schema.doc`), so a documented option needs a named type.
- `--help` is a question about argv, not a value in the record: ask it with
  `wantsHelp(argv)` before parsing.

A field is `Bool`, `Int64`, `String`, a validated type over `Int64` or
`String`, an `Option` of those, or one `Array<String>` for the positionals.
Anything else fails the generation with a sentence naming the field.

Inspect the synthesized module with:  vyrn emit-gen <file>

## CliHit

```vyrn
type CliHit = { field: String, value: String }
```

One option seen in argv: the field it fills, and the text it carried (`""`
for a flag).

## CliRead

```vyrn
type CliRead = { hits: Array<CliHit>, free: Array<String>, issues: Array<Issue> }
```

What one walk of argv found: the options, the free arguments, and the
problems with the argv itself (unknown option, missing value). The generated
parser raises the problems with a value.

## wantsHelp

```vyrn
fn wantsHelp(argv: Array<String>) -> Bool
```

Whether `--help` or `-h` appears before a `--` terminator. Ask it before
parsing: a program asked for help has not given the required options.

## cli

```vyrn
fn cli(module: String) -> String
```

Emits a module exporting `parse<Name>` and `help<Name>` for every exported
record type `module` declares.
