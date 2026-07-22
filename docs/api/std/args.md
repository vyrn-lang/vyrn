# std/args

std/args — CLI argument parsing over `args()` (RFC-0061). A pure-Vyrn library,
zero compiler support. It replaces hand-rolled argv index arithmetic with four
probes: `flag` (boolean presence), `opt` (`--name value` / `--name=value`),
`positionals` (the non-flag tokens), and `rest` (a `--` passthrough tail).

Deliberately the smallest library that deletes the hand-rolling, NOT clap:
there is no spec/schema, no generated `--help`, and no unknown-flag rejection —
the caller validates what it cares about. Flag/option NAMES are matched
verbatim (the caller writes the dashes), so `-v` works exactly like
`--verbose`; short-flag bundling (`-abc`) is out of scope.

A token starting with `-` (single or double dash) before a literal `--`
terminator is a flag/option token; a `--` ends flag parsing and everything
after it is positional/`rest` verbatim (including `-x` tokens).

## Args

```vyrn
type Args = { argv: Array<String> }
```

A snapshot of the command-line arguments — argv AFTER the program name, the
same slice every backend's `args()` returns. Build it with `cli()` (the live
args) or `cliOf(list)` (any token list — what the tests use).

## cli

```vyrn
fn cli() -> Args
```

The live arguments as an `Args` snapshot. `cli()` is exactly `cliOf(args())`.

## cliOf

```vyrn
fn cliOf(list: Array<String>) -> Args
```

An `Args` over an explicit token list — the constructor tests and callers that
already hold a token slice use.

## flag

```vyrn
fn flag(a: Args, name: String) -> Bool
```

Whether `name` is present anywhere before the `--` terminator, spelled exactly
(e.g. `flag(a, "--verbose")` or `flag(a, "-v")`).

## opt

```vyrn
fn opt(a: Args, name: String) -> Option<String>
```

The value of option `name`, or `None`. Two forms, before the `--` terminator:
- `--name=value` — everything after the first `=` (may be empty);
- `--name value` — the NEXT token, but ONLY if it does not itself start with
  `-` (a `-`-prefixed following token is NOT consumed, so the option is
  valueless → `None`).

The FIRST occurrence wins; later duplicates are ignored (deterministic).

There is no `intOpt` — `parse` composes:
  `match opt(a, "--port") { Some(s) => parse(s), None => None }`.

## positionals

```vyrn
fn positionals(a: Args) -> Array<String>
```

Every token that is neither a flag/option token nor consumed as an option
value, in order. A `-`-prefixed token is a flag/option token; its immediately
following non-`-` token (the `--name value` form) is treated as its value and
skipped too — so put free positionals BEFORE flags, or after a `--`. A literal
`--` ends flag parsing: every token after it is positional verbatim, including
`-x` tokens.

## rest

```vyrn
fn rest(a: Args, terminator: String) -> Array<String>
```

The tokens strictly after the first `terminator` (`"--"`), for
`tool -- passthrough args` shapes. Empty when the terminator is absent.
