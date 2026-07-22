# RFC-0061 — `std/args`: CLI Argument Parsing

- **Status:** Locked design
- **Depends on:** RFC-0014 (`args()` — the raw argv), RFC-0046
  (`std/strings`), RFC-0060 (`if let`/`break` — the examples read better
  with them, and vlog's migration uses them)
- **Evidence (dogfood):** `examples/vlog.vyrn` (the CLI dogfood) parses
  argv by hand — index arithmetic, prefix checks, and a friction-report
  entry asking for exactly this. Every future CLI example will re-roll it
  otherwise.

A pure-Vyrn library over `args()`. Zero compiler changes.

---

## Surface

```vyrn
import { cli, flag, opt, positionals, rest } from "std/args"

fn main() -> Int64 {
    let a = cli()                          // snapshot of args()
    let verbose = flag(a, "--verbose")     // Bool
    let name = opt(a, "--name")            // Option<String>
    let files = positionals(a)             // Array<String>
    ...
}
```

- `cli() -> Args` — a snapshot record over `args()` (also constructible
  from any `Array<String>` via `cliOf(list)` — that is what the tests
  use; `cli()` is just `cliOf(args())`).
- `flag(a, "--verbose") -> Bool` — present anywhere before the `--`
  terminator, as `--verbose` exactly.
- `opt(a, "--name") -> Option<String>` — accepts `--name value` (next
  token, even if it starts with `-`? NO — a following token that starts
  with `-` is NOT consumed; the option is valueless → `None`) and
  `--name=value` (everything after the first `=`, may be empty). First
  occurrence wins; later duplicates are ignored (deterministic, documented).
- `positionals(a) -> Array<String>` — every token that is not a
  flag/option token and not consumed as an option value, in order. A
  literal `--` terminates flag parsing: everything after it is
  positional verbatim (including `-x` tokens).
- `rest(a, "--") -> Array<String>` — the tokens after the terminator
  only (for `tool -- passthrough args` shapes).
- A token starting with `-` (single or double dash) before the
  terminator is a flag/option token. Short-flag BUNDLING (`-abc`) is out
  of scope; `-v` works as a flag name exactly like `--verbose` does
  (names are matched verbatim — the caller writes the dashes).
- No spec/schema, no auto-help, no unknown-flag rejection in v1 — the
  caller validates what it cares about. This is deliberately the smallest
  library that deletes vlog's hand-rolling, not clap.

## Numbers

No `intOpt` — `parse(s) -> Option<Int64>` already exists and composes:
`match opt(a, "--port") { Some(s) => parse(s), None => None }`. The doc
comment shows this idiom instead of widening the API.

## Migration & proof

- `examples/vlog.vyrn` migrates its hand-rolled argv handling to
  `std/args` (using `if let` where it reads better). Its parity behavior
  (with the existing fixtures) must stay byte-identical.
- A new parity example `examples/argsdemo.vyrn` exercising every rule
  above (flags, `=`-values, unconsumed `-` values, duplicates, `--`
  terminator, rest). If the parity harness lacks an argv-fixture
  convention (`.args` alongside `.stdin`), ADD one, mirrored across all
  three backends (interp/native argv, wasmtime args) — byte-identical
  including with an empty argv.

## Verification

1. Inline tests over `cliOf([...])` for every rule: exact-name matching,
   `--name=`, `--name -x` (value NOT consumed), first-wins duplicates,
   `--` semantics, `rest`.
2. vlog parity unchanged; argsdemo three-way byte-identical (new fixture
   convention if needed, exercised on all backends).
3. Full suite + LSP + parity green; `vyrn fmt --check` clean; 0 new
   clippy warnings; std-only ⇒ no LSP redeploy (state the unchanged
   hash) unless the harness change touched compiler/ (then hash-verify).

## Out of scope

Spec-driven parsing, generated `--help`, unknown-flag errors, short-flag
bundling, subcommand routing, and typed option decoding beyond the
`parse` idiom.
