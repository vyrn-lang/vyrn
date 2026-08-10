# RFC-0098 — `std/cli`: the command line is a record type

- **Status:** **M1 landed.** Two of its claims died to measurement: M1 was
  specified as "pure generator, zero compiler change" and could not be, because
  the native backend refuses a fallible construction over a `String` base and an
  option type over `String` is what a command line hands you; and the milestone's
  own dogfood — migrating `examples/vlog.vyrn` — belongs to M2, because vlog is
  nothing but subcommands and M1 has none. M2 to M7 are stated below with their
  blockers, as `docs/research/cli-lib.md` framed them.
- **Depends on:** RFC-0003 (validated types, and `T?(v)`), RFC-0009
  (`Validation<T>` / `Issue`), RFC-0021 (generator imports, `moduleInterface`),
  RFC-0031 (the reachable type closure), RFC-0054 (code quotes, `lex()`),
  RFC-0060 (`if let`), RFC-0061 (`std/args`, and the `.args` parity fixture),
  RFC-0086 (the compiler asks the type).
- **Research:** `docs/research/cli-lib.md` — the census of six CLI libraries, the
  three rules all six agree on, and the gap census this RFC works from.
- **Principle:** one declaration produces the parser and the help. The rule that
  refuses a bad `--port` is the rule the program relies on, because it is the
  same rule.

---

## The question

Six CLI libraries agree on one thing: **one declaration produces the parser, the
help, and the completions.** clap reads a struct and its doc comments; picocli
reads annotations; cliffy reads a usage string. None keeps a second help
document, because two documents drift.

Vyrn already owns every part of that except one, and it owns them better. A
`gen fn` reflecting `moduleInterface` is a stronger mechanism than a proc macro —
it is interpreted, sandboxed, content-addressed and pinned (RFC-0021). And the
value check does not need a `value_parser` beside the field: the field's type IS
the check.

```vyrn
// examples/lib/serve.vyrn — the whole command, and nothing but the command.

/// The TCP port to listen on.
export type Port = Int64 where value >= 1 && value <= 65535

/// The directory to serve. Must not be empty.
export type Root = String where value.byteLength > 0

/// Serve a directory over HTTP.
export type Serve = {
    port: Port,
    root: Root,
    host: Option<String>,
    verbose: Bool,
    files: Array<String>,
}
```

```vyrn
import { cli, wantsHelp } from "std/cli"
import { parseServe, helpServe } from cli("./lib/serve")
```

The generator emits, for every exported record type in the reflected module:

```vyrn
export fn parseServe(argv: Array<String>) -> Validation<Serve>
export fn helpServe() -> String
```

---

## The mapping rules

Every rule is decidable from the declaration, so the generator never guesses.
There is no `#[arg(..)]` equivalent, because Vyrn has no attributes and should
not grow them for this. Where a rule is not enough, name a type — the same
answer RFC-0086 gives everywhere else.

| field type | surface | missing |
|---|---|---|
| `Bool` | `--verbose` | `false` |
| `Option<T>` | `--host <value>` | `None` |
| bare `T` | `--port <value>` | an `Issue` at path `port` |
| `Array<String>` | the positionals, at most one field | empty |

- **The long name is the field name**, lowerCamelCase to kebab-case: `dryRun` is
  `--dry-run`. One rule, no attribute.
- **The short name is the first byte of the field name**, allocated in
  declaration order and skipped on collision. `-h` is reserved for `--help`
  rather than contested — a `--host` that answered `-h` would make
  `tool -h /srv` print the help and exit.
- **An unknown option is an `Issue`.** This is the difference from `std/args`: a
  spec exists, so silence would be wrong. So is a free argument in a command that
  declares no positionals.
- **A value option takes the next token verbatim.** `std/args` cannot know which
  options take a value, so it refuses to consume a `-`-leading token; here the
  spec says so, and `--port -1` is a port of -1.
- **Values are validated by construction.** `--port 99999` reaches
  `Port?(99999)`, which answers `None`, which becomes an `Issue` worded from that
  type's `Schema`. clap needs a `value_parser` for this and still keeps the
  domain rule in two places.
- **The first occurrence of an option wins**, as in `std/args` (RFC-0061). One
  word, one meaning across the two libraries.

### `--help` is a question about argv

The generator reserves `--help`/`-h` in the spec, so they are never "unknown".
It emits no help FIELD, because a program that asks for help has not given the
required options and its parse would answer `Invalid` for reasons nobody wants
to read. The caller asks first:

```vyrn
let argv = args()
if wantsHelp(argv) {
    print(helpServe())
    return 0
}
```

`wantsHelp` is a runtime function over argv alone: `--help` or `-h` before a
`--` terminator. A record that declares its own `help` field takes the flag
instead, and the generator adds nothing.

`--version` is not in M1. There is no version to print: nothing in a module says
what version the program is, and inventing a constant for the generator to bake
would be the second document this design exists to avoid.

### Errors are the library's own words

`Issue` carries a `key`, a `path` (the field) and a message. No internal name
reaches a message: option names are what the user typed, bounds are digits, and
the only user prose in the whole output — a type's `///` — appears in the help
text, baked through an RFC-0054 code quote so the compiler's own escaping owns it.

| key | message |
|---|---|
| `cli.unknown` | ``unknown option `--porrt` `` |
| `cli.value` | ``option `--port` needs a value`` / ``option `--verbose` takes no value`` |
| `cli.missing` | ``required option `--root` is missing`` |
| `cli.unexpected` | ``unexpected argument `stray` `` |
| `cli.number` | ``option `--port` expects a whole number`` |
| `cli.invalid` | ``option `--port` expects a whole number, 1 to 65535`` |

`Validation` accumulates, so one argv that breaks four rules reports four
issues, each with the field it belongs to.

### Help comes from the option's own type

Per-field `///` docs do not exist. `ast::Field` is `{ name, ty }`;
`Parser::take_docs` runs at declaration positions only and its own comment says a
stray doc comment inside a body is discarded; `render_type_decl` re-emits fields
as `name: Type`. So `lex()` over `TypeInfo.source` cannot recover what the parser
threw away.

M1 ships around it: an option's help text is the `///` above its own NAMED type,
which arrives as `TypeInfo.schema.doc`, plus that type's bound. The cost is that
a documented option needs a named type — and a named validated type is what
RFC-0003 asks for anyway, and it is reusable. M3 below is the compiler change
that lifts this, and it is not a CLI feature.

```
Serve a directory over HTTP.

Usage: serve [options] [<files>...]

Options:
  -p, --port <value>  The TCP port to listen on. (1 to 65535)
  -r, --root <value>  The directory to serve. Must not be empty. (at least 1 byte long)
      --host <value>
  -v, --verbose
  -h, --help          Show this help.
```

`--host` is `Option<String>` — no named type, so no prose. That is the rule
being visible rather than the help being broken.

---

## How it is built

`std/cli.vyrn` is one file in two halves, and the split is what keeps the
generator small.

**The runtime half** is ordinary Vyrn: `readArgv(opts, argv) -> CliRead` walks
argv once against a spec and answers the hits, the free arguments, and the
problems with argv itself. `cliFlag`, `cliValue`, `cliIssues`, `wantsHelp` and
four `Issue` constructors sit beside it. All of it is tested inline over plain
arrays, with no generation involved.

**The generation half** emits declarations and lookups and nothing clever: a
spec array, a parse function that is one `if let` per field, and a help string.
The field walk is `std/http:httpFields`' walk, not a new one — `lex()` over
`TypeInfo.source`, an identifier at brace depth 1 followed by `:`, and the tokens
after it up to the next top-level `,`. The comment there records why: hand-rolled
scanners got it wrong in `std/vyx`.

A record type in the reflected module is a command (`schema.base` reads
`record`); a named scalar type is an option type. A module with no exported
record fails the generation with a sentence saying so. So does a field whose type
a command line cannot spell, and a second `Array<String>`.

M1 understands `Bool`, `Int64`, `String`, a validated type over `Int64` or
`String`, an `Option` of those, and one `Array<String>`. `Float64` and the sized
integers are not refused on principle — they are simply not written yet, and each
is a branch plus a test.

### Every generated local carries its field's name

Two sibling `if let`s that bind one name, where the first is moved into a
validated construction, are refused as a single moved binding:

```vyrn
if let Some(t) = give("x") {
    if let Some(ok) = Root?(t) { p = ok }
}
if let Some(t) = give("y") {   // `t` was moved here into `Root` / ... used again here
    q = t
}
```

The two `t`s are different bindings in disjoint scopes. This is recorded rather
than fixed: it is a scoping question in `movecheck`, the corpus has no
hand-written instance, and a generator that names its locals after its fields
never meets it.

---

## M1 was not a zero-compiler-change milestone

The research note said "pure generator, zero compiler change" and its own example
declared `Root = String where value.byteLength > 0`. Those two sentences cannot
both hold. Two defects sat under that line, and the first one hid the second.

### 1. Native fallible construction was `Int64` only

`gen_try_construct` (text-IR backend) refused any base but `Int64`, with
`use vyrn run`. The direct wasm backend accepted **any scalar** already, and a
`String` is one there. So the two compiling backends disagreed about a language
feature, and the one that refused is the one every `vyrn build` uses.

The fix is the direct backend's rule, written in the other backend's terms: the
predicate's `value` binds at the declared base's own LLVM type, and a `String`
payload is its pointer — the same word a `Some(s)` carries, `ptrtoint` and back.
`Int64` and `String` are the two bases; anything else keeps a refusal, reworded.

**A refused `String` leaks its buffer.** The payload word is stored whatever the
tag says, so a `None` from `Root?("")` holds a pointer nothing releases — one
buffer per refused option, in a program that is about to print a usage error and
exit. The direct backend has always done the same. It is written down rather than
branched around.

### 2. `Age?(n)` was read as an `Age`, not an `Option<Age>`

`declared.rs` answered `Type::Named(name)` for `Expr::TryConstruct`. Every other
reading of that expression says `Option<Named>` — the checker's, and the direct
backend's own arm. The wrong answer cost nothing while the corpus only refined
numbers: `own` gave the `if let` scrutinee a release at the payload's width, and
an `Int` payload owns no heap, so the release was a no-op.

The first `String`-based option type made it an access violation on the first
line of the program: the release loaded the sum's TAG word as a `String` pointer
and freed it. One line fixes it, and `examples/validatestr.vyrn` is what keeps it
fixed — the accepted path, the refused path, and the path where the value arrives
out of another `Option`, byte-identical on all three engines.

---

## The vlog migration belongs to M2

The research note's M1 evidence asks for `examples/vlog.vyrn` migrated from
`std/args` to `std/cli`. It is not here, and the reason is M1's own boundary.

vlog is a subcommand program: the first positional selects `count`, `filter`,
`tail`, `fmt` or `stats`, and M1 has no subcommands. A per-subcommand record can
be parsed against the whole argv, but then `--file` — vlog's GLOBAL option — is
either duplicated into every subcommand's record or rejected as unknown by all of
them. Unknown-flag rejection is the feature; turning it off for one program would
be the wrong lesson.

vlog's `filter` is the first customer M2 should have, and its `--level` is the
best case for the design: `parseLevel`/`levelName` plus the `hasContains` bool
are 30 lines of hand-rolling that one validated type and one `Option<String>`
replace. It waits for the enum shape.

---

## Milestones

**M1 — one command record. LANDED.** Flags, options, positionals, unknown-flag
rejection, `--help`, `Validation` errors. No subcommands, no completions, no
colour.

*Evidence, as landed:*

- `examples/clidemo.vyrn` + `clidemo.args` — a three-way parity citizen that
  parses a valid argv and prints both the parse and the help the same
  declaration produced.
- `examples/clifail.vyrn` + `clifail.args` — its sibling: one argv breaking four
  rules at once, four accumulated issues on stderr in the library's own words,
  the help on stdout, exit code 2, byte-identical on all three backends.
- `examples/validatestr.vyrn` — the two compiler fixes, pinned.
- `compiler/vyrn-cli/tests/cli.rs` — two rows on the DEFAULT gate, because
  parity is `--ignored` and compares the three engines against each other: three
  engines agreeing on a message that leaked an internal name would be green. One
  row runs the inline tests; the other reads `clifail.args` and pins the four
  messages, the exit code, the help, and the absence of `@`, `$` and `__vyrn_`
  from anything a user reads.
- 10 inline tests in `std/cli.vyrn` over `readArgv` and the comptime helpers:
  the two value forms, the short name, the dash-leading value a spec makes
  legal, unknown options, a value with no value, a flag given one, the `--`
  terminator, first-occurrence-wins, `wantsHelp`, kebab and short-letter
  derivation, `Option<T>` reduction, and every bound phrasing.
- `vyrn emit-gen examples/clidemo.vyrn` is byte-identical across runs and across
  the two generation engines (the `genwasm` sweep discovers both new examples
  rather than listing them).

**M2 — subcommands and completions.** An enum whose variants carry record
payloads; the first positional selects the variant by its lowercased name; a
nullary variant takes no further arguments; nesting is nesting.
`completions<X>(shell)` for bash, zsh and fish. `examples/vlog.vyrn` migrates
here, `filter` first.
*Evidence:* a two-level subcommand example in the parity corpus; each generated
completion script checked by its own shell's parser in CI (`bash -n`, `zsh -n`,
`fish -n`) — the cheapest real check, and it catches quoting faults, which is the
whole risk; vlog's existing parity output unchanged.
*Known blocker:* enum variants carry no docs either (`EnumVariant` is
`{ name, payload }`), so a subcommand's one-line summary comes from its payload
record's own `///` and a nullary variant such as `Version` gets none. M3 should
cover variants in the same change.

**M3 — field and variant docs in reflection.** `Field.doc`, `EnumVariant.doc`,
`TypeInfo.fields: Array<FieldInfo { name, spelling, doc, schema }>`, and
`render_type_decl` round-tripping them. **Not a CLI milestone.** It pays the LSP
hover, `vyrn doc`, the JSON-Schema emitter's `description` and `std/openapi` at
the same time, and it removes the `lex()`-over-`source` field walk from
`std/http`, `std/graphql` and this RFC's own generator.
*Evidence:* a round-trip test — a record with field docs, reflected and
re-rendered, re-parses to the same declaration with the docs intact; LSP hover
shows a field's doc; `jsonSchema` emits a `description` per field; the two
generators that drop their scanners produce byte-identical output before and
after.
*This is the one hard blocker on clap's best feature*, and it is why M1's help
text asks for a named type per documented option.

**M4 — `std/term` styling, plus three builtins.** `hostIsTty(fd)`,
`hostGetEnv(name)`, `printRaw(fd, s)`, each on the RFC-0043 precedent: a
module-private `extern fn host*` in `std/term.vyrn`, mapped by
`host_boundary_extern` to a `__vyrn_*` symbol, five edits and a browser shim
each. Colour off when the output is not a terminal, off when `NO_COLOR` is set.
*Evidence:* the parity harness pipes stdout, so `hostIsTty` is false and the
three backends stay byte-identical with no special case — that property IS the
acceptance test.
*Known blocker:* **there is no environment-variable builtin at all.** No Vyrn
program can read `NO_COLOR`, `TERM` or `COLUMNS`, and none can offer clap's
env-backed options either. It is the highest-value row in the whole ladder and it
is not a terminal feature; it deserves its own answer, including whether ambient
authority fits the capability model.

**M5 — line prompts and progress.** `confirm`, `input`, `inputValid`; `bar`,
`spinner`, drawing to stderr and hidden when the output is not a terminal —
indicatif's draw target, so no caller branches.
*Evidence:* a `.stdin` fixture drives the prompts in parity; the same run
produces no progress output at all, proving the hidden target.

**M6 — raw mode, key reads, selection prompts.** `hostSetRawMode`,
`hostReadKey`, and `select`/`multiSelect`/`password`.
*Known blocker, and it must be decided FIRST:* **Vyrn does not unwind.** A trap
prints and exits, so a program that set raw mode leaves the terminal in raw mode
after one. ratatui documents the same failure and answers it with a panic hook
its host language provides; Vyrn has none. The trap path already runs a canonical
printer before exiting and can call one registered `fn()` first — small, and
generally useful to any program holding an OS resource. It is probably its own
small RFC, and it lands **before** raw mode, not after.

**M7 — `std/tui`.** Buffer, diff, one-axis layout, six widgets, and a
`TestBackend` that renders into a buffer with no terminal at all — which is what
makes a TUI a three-way parity citizen.

---

## Alternatives refused

**A command per exported function, picocli-style.** Refused: `FnInfo` has no
`doc` (`MemberInfo` from `contractOf` has one and `FnInfo` from
`moduleInterface` does not), so a function-shaped CLI has no help channel at all.
The record shape has one, through its option types.

**A separate help table beside the declaration.** Refused: two documents drift.
All six libraries in the census agree, and it is the one thing they agree on
without exception.

**`#[arg(short = 'p')]`-style attributes.** Refused: Vyrn has no attributes and
should not grow them for a command line. Everything is a rule over the
declaration; where a rule is not enough, name a type.

**A marker alias `type Positional<T> = Array<T>` for the positional field.**
Refused for M1: a generic alias to a built-in container is untested, and "the one
`Array<String>` field" is a rule that needs no new language feature. A second one
fails the generation with a sentence rather than picking.

**Deriving no short flags at all.** Considered. picocli and clap both make the
author write the short name, and a derived one changes when a field is renamed,
which changes a user's command line. M1 derives them because the alternative
today is a named type per short flag, which is worse; the marker belongs on a
field doc, which is M3. Recorded as open.

**Building the parser on `std/args`' `positionals`.** Refused: that heuristic
exists because `std/args` has no spec — it guesses that a `-`-token consumes the
next non-`-` token. With a spec the guess is unnecessary and wrong. `std/cli`
walks argv itself, in one pass, and `std/args` stays the smaller library it was
written to be.

**Escaping the user's `///` prose by hand.** Refused: `std/graphql` and `std/tw`
both deleted their hand-rolled escapers for an RFC-0054 code quote, and the audit
in `docs/research/cli-lib.md`'s neighbourhood found real mis-escapes in the
generators that kept one. The help text is baked through `\{text}` in expression
position, so a quote or a backslash in a doc comment cannot become code.

---

## Open questions

1. **Should short flags be derived at all?** See above. The answer probably
   arrives with M3's field docs, as a marker on the field.
2. **Where does the exit code come from?** clap exits 2 on a usage error. `main`
   returns `Int64`, so the program chooses, and both examples here choose 2.
   Should `std/cli` export a `usageExit` constant so every Vyrn CLI agrees?
3. **Does `std/cli` compose with `std/i18n`?** Both are generators. Help text as
   translation keys would need one generator's output to be another's input.
   RFC-0021 does not forbid it; nothing has tried it.
4. **Does the refused-`String` payload leak deserve a branch?** One buffer per
   refused option, on a path that is about to exit. Freeing it means branching on
   the tag in both backends, and the two must agree.
