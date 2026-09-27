# std/args

std/args: command-line argument parsing over `args()`, in Vyrn.

Four probes: `flag`, `opt` (`--name value` or `--name=value`),
`positionals` and `rest` (a `--` passthrough tail). There is no spec, no
generated `--help` and no unknown-flag refusal; `std/cli` has those. Names
match verbatim, dashes included, and `-abc` is one token. A token that
starts with `-` before the first `--` is a flag or option token; every token
after `--` is positional.

## Args

```vyrn
type Args = { argv: Array<String> }
```

The command-line arguments after the program name.

## cli

```vyrn
fn cli() -> Args
```

## cliOf

```vyrn
fn cliOf(list: Array<String>) -> Args
```

An `Args` over an explicit token list.

## flag

```vyrn
fn flag(a: Args, name: String) -> Bool
```

Whether `name` appears, spelled exactly, before the `--` terminator.

## opt

```vyrn
fn opt(a: Args, name: String) -> Option<String>
```

The value of option `name` before the `--` terminator, or `None`. The first
occurrence wins. `--name=value` gives everything after the first `=`, which
may be empty. `--name value` gives the next token unless it starts with `-`;
then the option has no value and the answer is `None`.

## positionals

```vyrn
fn positionals(a: Args) -> Array<String>
```

Every token that is neither a flag or option token nor an option's value, in
order, then every token after `--`. A non-`-` token after an option token
without `=` counts as its value, so put free positionals before flags or
after `--`.

## rest

```vyrn
fn rest(a: Args, terminator: String) -> Array<String>
```

The tokens after the first `terminator`; empty when it is absent.
