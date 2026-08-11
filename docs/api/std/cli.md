# std/cli

std/cli — the command line IS a record type (RFC-0098, M1).

  import { cli } from "std/cli"
  import { parseServe, helpServe } from cli("./serve")

One `gen fn` reflects a module of type declarations and RETURNS a module that
exports, for every exported RECORD type in it, `parse<Name>(argv) ->
Validation<Name>` and `help<Name>() -> String`. The compiler knows nothing
about command lines: the generator is comptime-pure Vyrn string building, the
`std/openapi` precedent, and the walk over a record's fields is the
`std/http:httpFields` walk (`lex()` over `TypeInfo.source` at brace depth 1).

The mapping is a rule over the declaration, never an attribute:

| field type | surface | missing |
|---|---|---|
| `Bool` | `--verbose` | `false` |
| `Option<T>` | `--host <value>` | `None` |
| bare `T` | `--port <value>` | an `Issue` at path `port` |
| `Array<String>` | the positionals, at most one field | empty |

- The long name is the field name in kebab-case: `dryRun` is `--dry-run`.
- The short name is the first byte of the field name, taken in declaration
  order and skipped on collision. `-h` is reserved for `--help`.
- An unknown option is an `Issue`. This is the difference from `std/args`: a
  spec exists, so silence would be wrong.
- A value is validated BY CONSTRUCTION. `--port 99999` reaches `Port?(99999)`,
  which answers `None`, which becomes an `Issue` worded from that type's
  `Schema` — so the check the parser runs and the check the program relies on
  are one check.
- Help text per option comes from the `///` above the option's own NAMED type
  (`TypeInfo.schema.doc`). Per-FIELD `///` docs are discarded by the parser
  today, so a documented option needs a named type — which is what RFC-0003
  asks for anyway. RFC-0098 M3 is the compiler change that lifts this.
- `--help` is a question about argv, not a value in the record, so the caller
  asks it with `wantsHelp(argv)` before parsing.

M1 understands `Bool`, `Int64`, `String`, a validated type over `Int64` or
`String`, an `Option` of those, and one `Array<String>` for the positionals.
Anything else fails the generation with a sentence naming the field.

Inspect the synthesized module with:  vyrn emit-gen <file>

## CliOpt

```vyrn
type CliOpt = { long: String, short: String, field: String, takesValue: Bool }
```

One declared option, as the generated spec describes it to the walk.

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
problems with the argv ITSELF (unknown option, missing value). Problems with
a VALUE belong to the type that refused it, and the generated parser raises
those.

## readArgv

```vyrn
fn readArgv(opts: Array<CliOpt>, argv: Array<String>) -> CliRead
```

Walk `argv` against `opts`.

A token starting with `-` (and not a bare `-`) is an option; `--name=value`
carries its value inline, otherwise an option that takes a value takes the
NEXT token verbatim — a spec exists, so `--port -1` is a port of -1 and not a
missing value. A literal `--` ends option parsing and everything after it is
a free argument. The FIRST occurrence of an option wins, as in `std/args`.

## cliFlag

```vyrn
fn cliFlag(r: CliRead, field: String) -> Bool
```

Whether the flag field `field` was seen.

## cliValue

```vyrn
fn cliValue(r: CliRead, field: String) -> Option<String>
```

The text the option field `field` carried, first occurrence wins.

## cliIssues

```vyrn
fn cliIssues(r: CliRead) -> Array<Issue>
```

The walk's own problems, as the accumulator a generated parser starts from.

## wantsHelp

```vyrn
fn wantsHelp(argv: Array<String>) -> Bool
```

`--help` or `-h` anywhere before a `--` terminator. Asked BEFORE parsing:
help is a question about argv, and a program that asks for it has not given
the required options.

## cliMissing

```vyrn
fn cliMissing(field: String, long: String) -> Issue
```

A required option nobody gave.

## cliNotNumber

```vyrn
fn cliNotNumber(field: String, long: String) -> Issue
```

A value that is not a whole number.

## cliRefused

```vyrn
fn cliRefused(field: String, long: String, want: String) -> Issue
```

A value the field's own type refused. `want` is that type's rule, worded from
its `Schema` by the generator.

## cliUnexpected

```vyrn
fn cliUnexpected(value: String) -> Issue
```

A free argument in a command that declares no positionals.

## cli

```vyrn
fn cli(module: String) -> String
```

`cli(module)` — emit a module exporting `parse<Name>` and `help<Name>` for
every exported record type `module` declares.
