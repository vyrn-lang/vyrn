# RFC-0061 — `std/args`: CLI Argument Parsing

- **Status:** Implemented
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

## As landed

`std/args.vyrn` is a pure-Vyrn library — **zero compiler changes**. It exports
`Args`, `cli`/`cliOf`, and `flag`/`opt`/`positionals`/`rest`, over `args()`.

- `opt` handles `--name=value` (everything after the first `=`, empty allowed)
  and `--name value` (the next token, NOT consumed when it starts with `-`), with
  first-occurrence-wins duplicates. `flag` is exact-name presence before `--`.
- **`positionals` heuristic (documented):** with no spec it cannot know which
  flags take a value, so it uses the rule consistent with `opt` — a `-`-token
  greedily consumes its immediately-following non-`-` token as a value. So put
  free positionals BEFORE flags, or after a `--`. Everything after a literal `--`
  is positional verbatim; `rest(a, "--")` returns just that tail.
- 8 inline tests over `cliOf([...])` cover every rule (exact-name, `--name=`,
  `--name -x` not consumed, first-wins duplicates, `--` semantics, `rest`, empty
  argv).

**Migration & harness.** `examples/vlog.vyrn` dropped its hand-rolled
`getFlag`/`Flag` for `std/args` (`cli`/`opt`/`positionals`/`flag`), reading the
`--file`/`--level`/`--contains` options via `if let` and taking the subcommand
from the first positional; its parity with `vlog.stdin` is byte-identical
(interp == native after the documented CRLF normalization). A new
`examples/argsdemo.vyrn` (+ `argsdemo.args`) exercises every rule and is a
three-way parity citizen.

The parity harness gained the **`.args` fixture convention**: `examples/<name>.args`
holds one argv token per line, forwarded byte-identically to all three backends —
`vyrn run <file> <args>`, the native `<exe> <args>`, and `wasmtime run … <module>
<args>` (guest argv after the module path); absent fixture ⇒ empty argv. This
touched `compiler/vyrn-cli/tests/parity.rs`, so it is exercised by the same
full-suite run; no compiler/frontend code changed, so **the LSP binary is
unchanged** by this RFC (RFC-0060's rebuild covers the shared parser/checker; its
hash is `b14410c13de9daab3ef81da0fdc8d139f2f2489430a66cf826db087a058b9aba`).
