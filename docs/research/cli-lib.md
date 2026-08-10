# CLI — a library census and a Vyrn design sketch

- **Status:** Research note. Not an RFC. No implementation.
- **Scope:** Part 1 is a census of six CLI libraries. Part 2 is a design sketch
  in three layers: `std/cli` (argument parsing), `std/term` (styling, prompts,
  progress), and a TUI. Part 3 is the gap census, the milestone ladder, and the
  open questions.
- **Related:** RFC-0003 (validated types), RFC-0008 (logging sinks), RFC-0009
  (`Validation<T>` / `Issue`), RFC-0012 (`extern`), RFC-0014 (input I/O),
  RFC-0021 (generator imports, `moduleInterface`), RFC-0031 (reachable type
  closure), RFC-0037 (stored closures), RFC-0038 (contract exports — the two
  recorded reflection gaps), RFC-0043 (time — the host-boundary extern
  precedent), RFC-0054 (`lex()`), RFC-0061 (`std/args`), RFC-0094 (a builtin is
  a declaration), `rfcs/census-builtins.md`

---

## Part 0 — What Vyrn has today

Read this first. Every claim below is read from the file named.

| Facility | Where | Note |
|---|---|---|
| `args() -> Array<String>` | census-builtins.md, module-extern bucket | argv after the program name |
| `std/args` | `std/args.vyrn` | `cli`/`flag`/`opt`/`positionals`/`rest`. No spec, no help, no unknown-flag rejection. Deliberate (RFC-0061) |
| `readLine() -> Option<String>` | census-builtins.md | line at a time, buffered |
| `print(x)` | `interp.rs:4203` | **`println` only.** There is no write without a newline |
| stderr | RFC-0008 | only through `logger(name).warn(..)`, which prefixes `[LEVEL] name: ` |
| exit code | `fn main() -> Int64` | a parse failure can return a code |
| clock | `std/time.now()`, `monotonic()` | needed by progress and spinners |
| `gen fn` | RFC-0021 | runs in the compiler, reads modules, returns Vyrn source |
| `moduleInterface(path)` | `schema_reflect.rs:13` | `ModuleInterface { functions, types }` |
| `lex(src) -> Array<Token>` | RFC-0054 | the compiler's own lexer, in gen code |
| `Validation<T>` / `Issue` | RFC-0009 | accumulating errors with a `path` and a message |
| `.args` parity fixture | RFC-0061 As-landed | one argv token per line, fed to all three backends |

And what is absent. Each was checked, not assumed.

- **No environment variables.** No `getEnv` in the 83-name census. A Vyrn
  program cannot read `NO_COLOR`, `TERM`, or `COLUMNS`.
- **No TTY test.** Nothing answers "is stdout a terminal".
- **No terminal size, no cursor control, no raw mode, no key reads.**
- **No unbuffered write.** `print` always ends the line.
- **No signals.** `SIGWINCH` and `SIGINT` are not observable.
- **No unwinding.** A trap prints and exits. Nothing runs on the way out.

---

## Part 1 — The census

### 1.1 clap (Rust)

| Feature | The design choice behind it |
|---|---|
| `#[derive(Parser)]`, `#[arg(short, long)]` | **The struct is the parse spec.** One declaration, no second document to drift |
| Field type drives behavior — `bool` is a flag, `Option<T>` optional, `Vec<T>` repeatable, bare `T` required | **The compiler asks the type.** The docs say clap "assumes some intent based on the type used" |
| `///` doc comments become help text; a blank line splits `-h` from `--help` | **The help is where the reader already looks.** No parallel help strings |
| `value_parser` — parse and validate before the value lands | **A typed value, not a string plus a check.** Retrieval is `get_one::<T>()` |
| `clap_complete` emits bash/zsh/fish/PowerShell/elvish scripts | **Completions are derived from the model, never authored** |

Admitted costs: help, colour, wrapping, and suggestions are all separate cargo
features, because the derived model is large. Dynamic completion is unstable.

### 1.2 ratatui (Rust)

| Feature | The design choice behind it |
|---|---|
| `terminal.draw(\|frame\| ..)` rebuilds the whole screen each frame | **Immediate mode.** The programmer holds no widget tree between frames |
| A `Buffer` of `Cell`s, diffed against the previous frame | **Redraw everything, send only the delta.** Cheap and flicker-free |
| `Layout` with `Length`/`Percentage`/`Min`/`Fill` constraints, solved by Cassowary | **Layout is declared and reconciled, not computed by hand** |
| `Widget::render(self, area, buf)` consumes the widget; state such as `ListState` is held outside | **Widgets are values, not objects.** What must survive a frame is kept out of the widget |
| `Backend` trait: crossterm, termion, termwiz, and `TestBackend` | **Terminal I/O is somebody else's crate.** A test asserts on a `Buffer` |

Admitted costs: an unsatisfiable constraint set degrades silently; a blocked
render loop freezes the screen; a panic leaves the terminal in raw mode unless
the program installs a hook; two crossterm versions in one build lose events.

### 1.3 dialoguer (Rust)

| Feature | The design choice behind it |
|---|---|
| Eight prompts: `Input`, `Password`, `Confirm`, `Select`, `MultiSelect`, `FuzzySelect`, `Sort`, `Editor` | **One builder per prompt, ended by `interact()`.** Control flow stays ordinary code |
| `with_theme` and the `Theme` trait | **Rendering is the only extension point.** Theme one prompt and you theme all |
| `interact_opt()` returns an `Option` | **Cancel is a value, not an error** |
| `validate_with` over `InputValidator` | Validation runs in the loop, so a bad answer re-asks |
| Terminal handling is delegated to the `console` crate | **Cursor, styling, and TTY detection are shared** with indicatif |

### 1.4 indicatif (Rust)

| Feature | The design choice behind it |
|---|---|
| `ProgressBar::new(len)`, `new_spinner()`, `hidden()`; draws to **stderr**, capped at 20 redraws a second | **The bar is a handle.** The caller calls `inc()` and never thinks about drawing |
| `ProgressDrawTarget` hides every bar when the output is not a terminal | **Degradation is a target, not a branch.** `hidden()` accepts every call and draws nothing |
| `ProgressStyle::template("{bar:40.cyan}  {eta}")` | **The layout of a bar is data.** Styles are shareable |
| `enable_steady_tick` runs an animator thread | **One clock owns the animation.** Manual ticks are then ignored |
| `MultiProgress`, `(0..n).progress()`, `wrap_read`/`wrap_write` | Progress attaches to what you already have |

Admitted costs: printing to stdout beside a live bar corrupts the display
unless you use `suspend`; `println` on a hidden bar prints nothing; three
finish calls leave three different residues.

### 1.5 picocli (Java)

| Feature | The design choice behind it |
|---|---|
| `@Command`, `@Option`, `@Parameters` | **The annotation is the model, and the model is reflectable.** `@Spec` hands it back at run time |
| One file, zero dependencies; can be included as source | **Ship the tool, not the jar.** This constraint explains the single-class design |
| `@Mixin` merges shared option sets; `@ArgGroup` declares exclusive and dependent groups | **Composition, and cross-option rules stated instead of coded** |
| `execute()` runs parse, validate, report, help, exit code | **One entry point owns the whole lifecycle**; replace it with `IExecutionStrategy` |
| `picocli-codegen` validates annotations at compile time and writes GraalVM `native-image` config | **Startup cost is a build problem**, so reflection-heavy Java still makes a fast binary |
| `Help.Ansi` auto-detects, honours `NO_COLOR` and `CLICOLOR`, and fixes Windows | Colour is a capability question, answered once |

Admitted costs: positional indexing depends on field iteration order, which
Java does not guarantee; interactive positionals must come before
non-interactive ones.

### 1.6 cliffy (Deno)

| Feature | The design choice behind it |
|---|---|
| `.option("-p, --port <port:number>", "..")` | **The usage string is the schema.** Name, arity, and type in one place |
| TypeScript infers the action callback's option object from that literal | **No second schema, no cast** |
| `.type()` registers a custom value type; built-ins include `file`, `secret`, `enum` | Types are extensible at the same level the built-ins live |
| `HelpCommand` and `CompletionsCommand` come from the same chain | **One model, three outputs**: parse, help, completions |
| Seven packages — `command`, `flags`, `prompt`, `table`, `ansi`, `keycode`, `keypress` | **Layers, separately installable.** Take the ANSI chain without the framework |

Admitted costs: the prompt `type` field takes a prompt object, not a name,
which the docs call a deliberate break from the npm convention. Every prompt in
a list needs a unique `name` or values overwrite.

### 1.7 What the six agree on

Three rules survive all six libraries.

1. **One declaration produces the parser, the help, and the completions.**
   clap uses the type and the doc comment. picocli uses the annotation. cliffy
   uses the usage string. None keeps a second help document.
2. **Degradation belongs in the sink, not in the caller.** indicatif's hidden
   draw target is the cleanest form. The calling code never asks whether it is
   on a terminal.
3. **Restore is the standing failure.** ratatui documents raw mode surviving a
   panic. indicatif documents a corrupted display. Any interactive layer needs
   an explicit teardown and a path that runs when the program dies.

Rule 1 is what `std/cli` should copy. Rule 2 is what `std/term` should copy.
Rule 3 is the one Vyrn cannot copy today, because Vyrn does not unwind.

---

## Part 2 — The Vyrn answer, in layers

### 2.1 Layer 1 — `std/cli`: the CLI is a record type

The thesis: clap's derive is the right idea, and Vyrn already owns every part of
it except one. `gen fn` + `moduleInterface` is a stronger reflection mechanism
than a proc macro, because it is interpreted, sandboxed, content-addressed, and
pinned (RFC-0021). The parts map like this.

| clap | Vyrn |
|---|---|
| `#[derive(Parser)]` on a struct | a record type in a module, reflected by a `gen fn` |
| field type decides flag or option | `TypeInfo.source` read with `lex()` |
| `value_parser` | the validated type itself, plus fallible construction `Port?(n)` |
| `#[arg(long = "..")]` | the field name |
| `///` on a field | **missing.** See 2.1.4 |
| `Subcommand` enum | an enum whose variants carry record payloads |
| `clap_complete` | plain string building in the same generator |
| parse errors | `Validation<T>` and `Issue` — already accumulating, already i18n-ready |

#### 2.1.1 The declaration

```vyrn
// serve.vyrn — the whole command, and nothing but the command.

/// The TCP port to listen on.
export type Port = Int64 where value >= 1 and value <= 65535

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

#### 2.1.2 The use

```vyrn
import { cli } from "std/cli"
import { parseServe, helpServe, completionsServe } from cli("./serve")

fn main() -> Int64 {
    return match parseServe(args()) {
        Valid(s) => run(s),
        Invalid(issues) => {
            let log = logger("serve")
            for i in issues {
                log.error("\{i.path}: \{i.message}")
            }
            print(helpServe())
            2
        },
    }
}
```

The generator emits three functions per command record.

```vyrn
// generated — inspect with `vyrn emit-gen`
export fn parseServe(argv: Array<String>) -> Validation<Serve>
export fn helpServe() -> String
export fn completionsServe(shell: String) -> String
```

#### 2.1.3 The mapping rules

Type-driven, in the house style. Every rule is decidable from the reflected
declaration, so the generator never guesses.

| Field type | Surface | Missing |
|---|---|---|
| `Bool` | `--verbose` flag | `false` |
| `Option<T>` | `--host <v>` | `None` |
| bare `T` (scalar) | `--port <v>` | an `Issue` at path `port` |
| `Array<T>` | positional list, at most one per record | empty |
| a record-typed field | a flag group with a `--group.field` prefix | per inner field |

Further rules.

- **Long names come from the field name**, lowerCamelCase to kebab-case:
  `dryRun` becomes `--dry-run`. One rule, no attribute.
- **Short names come from the first byte of the field name**, allocated in
  declaration order, skipped on collision. Deterministic, and the generated
  help states which fields got one.
- **Unknown flags are rejected.** This is the difference from `std/args`. A
  spec exists, so an unknown flag is an `Issue`, not silence.
- **`--help` and `--version` are added by the generator** unless the record
  already declares a field of that name.
- **Values are validated by construction.** `--port 99999` reaches
  `Port?(99999)`, which answers `None`, which becomes an `Issue` whose message
  is built from that type's `Schema` (`min`, `max`, `pattern`, `maxLength`).
  The check the parser runs and the check the program relies on are the same
  check. clap needs `value_parser` for this and still keeps the domain rule in
  two places.

**Subcommands are an enum.**

```vyrn
/// A static file server.
export type Cmd =
    | Serve(Serve)
    | Build(Build)
    | Version
```

The generator emits `parseCmd(argv) -> Validation<Cmd>`. The first positional
selects the variant by its lowercased name. A nullary variant takes no further
arguments. Nesting is nesting: a variant whose payload is another command enum
is a subcommand group.

#### 2.1.4 An honest evaluation of the reflection

This is the part to read before anyone writes the RFC.

**What works today, with no compiler change.**

- The field list. `TypeInfo.source` is canonical Vyrn source, and `lex()` reads
  it. `std/http:httpFields` (`std/http.vyrn:1374`) already does exactly this
  walk — identifier at brace depth 1 followed by `:` is a field — and the
  comment there records why: hand-rolled scanners got it wrong in `std/vyx`.
  `std/cli` reuses that walk, it does not invent one.
- The validation bounds. `Schema` is shallow, and the recorded gap
  (RFC-0038) is real: `ParamInfo.schema` carries only a scalar's own bounds. It
  does not block this design, because the bounds are not needed on the field —
  they are needed on the field's **named type**, and `iface.types` contains a
  `TypeInfo` for `Port` through the RFC-0031 reachable closure, with its
  `schema` filled. The generator looks up the spelling in `iface.types`. The
  shallow-`Schema` gap bites a design that reflects an anonymous inline record.
  It does not bite one that names its option types, and naming them is what
  Vyrn asks for anyway.
- The command's own summary line. `TypeInfo.schema.doc` carries the `///` above
  `type Serve`.
- Determinism, caching, and pinning. Free from RFC-0021.

**What does not work.**

- **Per-field `///` docs do not exist anywhere.** This was checked to the
  bottom. `ast::Field` is `{ name, ty }` and carries no doc
  (`ast.rs:267`). `Parser::take_docs` is called at declaration positions only,
  and its own comment says stray doc comments inside bodies "are simply
  discarded" (`parser.rs:1125`). `render_type_decl` re-emits fields as
  `name: Type` and emits no comments (`schema_reflect.rs:437`), so `lex()` over
  `TypeInfo.source` cannot recover what the parser threw away. No file in
  `std/` or `examples/` puts a `///` inside a record body; multi-field records
  such as `Scanner` use `//`.

  This is the one hard blocker on "the doc comment is the help text", which is
  clap's best feature.

  Three answers, in ladder order:

  1. **Ship M1 without it.** Help text per option comes from the option's own
     named type: `/// The TCP port to listen on.` above `type Port` reaches the
     generator as `TypeInfo.schema.doc`. Zero compiler change. It works today.
     The cost is that every documented option needs a named type, including
     plain `String` ones. That cost is not all cost — a named validated type is
     what RFC-0003 wants anyway, and the type is reusable.
  2. **Then make the compiler carry it.** `Field` gains `doc: Option<String>`;
     the record parser calls `take_docs` before each field; `render_type_decl`
     re-emits it; `TypeInfo` gains
     `fields: Array<FieldInfo { name, spelling, doc, schema }>`. The change is
     small and it is not a CLI feature — it pays the LSP hover, `vyrn doc`, the
     JSON-Schema emitter's `description`, and `std/openapi` at the same time.
     It also removes the `lex()`-over-`source` walk from `std/http` and
     `std/graphql`.
  3. Do not build a separate help table. Two documents drift. All six libraries
     agree on that.

- **`FnInfo` has no `doc`.** The recorded gap (RFC-0038) is confirmed:
  `MemberInfo` from `contractOf` has a `doc` field and `FnInfo` from
  `moduleInterface` does not (`schema_reflect.rs:13` and 33). It does not block
  a record-shaped CLI. It blocks the other possible shape — a command per
  exported function, picocli-style — so this design does not use that shape.
- **Enum variants have no docs either.** `EnumVariant` is `{ name, payload }`.
  A subcommand's one-line summary therefore comes from its payload record's
  own `///`, and a nullary variant such as `Version` gets none. Answer 2 above
  should cover variants in the same change.

**Verdict.** The clap-derive idea holds in Vyrn, and in two respects it is
better: the validation rule lives in the type instead of in a `value_parser`
beside it, and the error channel is `Validation` + `Issue`, which accumulates
and is already wired to i18n. The idea is weaker in exactly one respect, and it
is the documentation channel. M1 can ship around it. M3 should fix it properly,
for reasons that have nothing to do with the CLI.

#### 2.1.5 What the generator does not do

No `#[arg(..)]` equivalent, because Vyrn has no attributes and should not grow
them for this. Everything is a rule over the declaration. Where a rule is not
enough, name a type. That is the same answer RFC-0086 gives everywhere else.

### 2.2 Layer 2 — `std/term`: styling, prompts, progress

Split by what each part needs, smallest first. The split is not tidiness. It is
that the first part needs nothing at all and can ship immediately.

#### 2.2.1 Styling needs no builtin

An ANSI escape is a string. `print` already writes strings.

```vyrn
import { bold, red, dim, style, Style } from "std/term"

print(bold(red("error")) + ": " + msg)
```

`Style` is a record (`fg`, `bg`, `bold`, `dim`, `underline`); `style(s, st)`
wraps the text. This is pure string building, testable with `assertEq` on the
escape bytes, and it is byte-identical across all three backends.

The problem is that it must switch itself off. That needs two answers Vyrn
cannot give: is the output a terminal, and does the environment forbid colour.

#### 2.2.2 The builtin surface, smallest first

Every row below follows the RFC-0043 precedent exactly: a **module-private
`extern fn host*`** declared in `std/term.vyrn`, mapped by
`host_boundary_extern` (`vyrn-codegen/src/lib.rs:565`) to a `__vyrn_*` symbol.
Three such names exist today — `hostNowMillis`, `hostMonotonicNanos`,
`hostRandomSeed`. This is the cheap path, and it is the RFC-0094 shape: a
builtin that is a declaration.

Each new name costs five edits, measured against the `hostNowMillis` row:
`interp.rs:2217` (the interpreter arm), `codegen/lib.rs:567` + `:990` (the map
and the `declare`), `toolchain.rs:326` (the C shim body), `direct.rs:6702` (the
direct-wasm runtime binding), and `wasm.rs:922` (the import pin). Plus
`web/wasi-min.js` for the browser.

| # | Builtin | Unlocks | Cost |
|---|---|---|---|
| 1 | `hostIsTty(fd: Int64) -> Bool` | colour on or off; progress shown or hidden | `isatty`/`GetConsoleMode`. One shim function |
| 2 | `hostGetEnv(name: String) -> Option<String>` | `NO_COLOR`, `CLICOLOR`, `TERM`, `COLUMNS`; and clap's `env` options | `getenv`. Returns a `String`, so it crosses the ABI as a pointer |
| 3 | `printRaw(s: String)` | in-place redraw, spinners, `"Name: "` before a `readLine` | `fputs` with no newline. Not a host extern — this is `print`'s sibling and belongs in the same seeded-protocol arm |
| 4 | `hostTermSize() -> Int64` | help wrapping, bar width | `ioctl(TIOCGWINSZ)` / `GetConsoleScreenBufferInfo`. Pack rows and columns into one `Int64` to avoid a tuple ABI. Partly covered by `COLUMNS` from #2 |
| 5 | `hostSetRawMode(on: Bool) -> Bool` | key-at-a-time input | `tcsetattr` / `SetConsoleMode`. Carries the restore problem — see 2.2.5 |
| 6 | `hostReadKey() -> Int64` | select, multi-select, fuzzy, any TUI | one byte, unbuffered, `-1` at end of input. Useless without #5 |

Rows 1 to 3 are the whole of the "styling and progress" story. Rows 5 and 6
are the whole of the "interactive selection" story and cost more than the rest
put together. Row 4 is optional at every stage.

Note that **row 2 is the highest-value row and it is not a terminal feature.**
No Vyrn program can read an environment variable today. That blocks
`NO_COLOR`, and it also blocks the clap and cliffy pattern of an option that
falls back to an environment variable. It should be judged on its own.

#### 2.2.3 Prompts

dialoguer's eight prompts split cleanly along row 5.

**Line-mode prompts need only rows 1 to 3, plus `readLine`.**

```vyrn
import { confirm, input, password } from "std/term"

let name = input("Project name", Some("app"))       // -> String
let go = confirm("Overwrite?", false)               // -> Bool
```

`confirm` and `input` are a `printRaw` of the prompt, a `readLine`, a
validation, and a loop. `password` needs row 5 to stop the echo, so it is not
in this group.

**Selection prompts need rows 5 and 6.** `select`, `multiSelect`,
`fuzzySelect`, and `sort` all redraw a list on every keystroke.

The validator hook is a stored closure (RFC-0037), which exists:

```vyrn
export fn input(label: String, fallback: Option<String>) -> String
export fn inputValid(label: String, check: fn(String) -> Validation<String>) -> String
```

Not on a terminal, every prompt returns its default and prints nothing. That is
indicatif's rule applied to prompts: the caller does not branch.

#### 2.2.4 Progress

indicatif's model transfers whole, and it needs no new capability past row 3.

```vyrn
import { bar, spinner, tick, finish, Progress } from "std/term"

let mut p = bar(files.length, "{bar:40} {pos}/{len} {eta}")
for f in files {
    process(f)
    tick(p, 1)      // `fn tick(p: modify Progress, n: Int64)`
}
finish(p)
```

- Draws to **stderr**, like indicatif, so a piped stdout stays clean. Vyrn's
  stderr today is the logger with its `[LEVEL] name: ` prefix, so progress
  needs a raw stderr write. Make row 3 `printRaw(fd, s)` and take both streams
  in one builtin rather than two.
- Rate limits itself with `monotonic()` from `std/time`. No animator thread,
  so no steady tick in v1: a spinner advances when the program calls `tick`.
  That is the lazy version and it is enough for a build tool.
- `hostIsTty` false means every call is accepted and nothing is drawn. The
  hidden draw target, exactly.
- No `MultiProgress` in v1. Vyrn has `spawn`, and logging is already barred
  from spawned tasks, so a shared bar across tasks is a separate design.

#### 2.2.5 The restore problem, stated plainly

Vyrn does not unwind. A trap prints to stderr and exits. If the program set raw
mode, the terminal stays in raw mode after the trap. ratatui documents this
same failure and answers it with a panic hook that its host language provides.
Vyrn has no such hook.

Three options, in order of cost:

1. **Accept it in v1 and document it.** Raw mode is only entered by selection
   prompts and the TUI, and both are late milestones.
2. **A trap hook.** The trap path already runs a canonical printer before
   exiting. It can call one registered `fn()` first. This is small and it is
   also generally useful.
3. Full unwinding. Refused elsewhere in the project and not worth reopening for
   a terminal.

Option 2 is the right one, and it should be decided **before** row 5 lands, not
after.

#### 2.2.6 The browser

`wasi-min.js` already degrades an absent host to an empty world with canonical
errors. `std/term` follows it: `hostIsTty` false, `hostTermSize` zero,
`hostGetEnv` `None`, `hostSetRawMode` false, `hostReadKey` `-1`. Styling still
works, because an escape sequence is just bytes and a browser terminal emulator
renders it. Progress is hidden. Prompts return defaults. No branch in any
caller, and no `WASM_ONLY` parity list entry needed.

### 2.3 Layer 3 — the TUI

**Feasible. Later. And smaller than it looks, if it copies ratatui's core and
refuses ratatui's extensibility.**

What transfers with no language work:

- `Buffer` is a record holding `Array<Cell>` plus width and height. `Cell` is
  `{ ch: String, style: Style }`. The diff is a loop over two arrays.
- `Rect`, and the layout split. Cassowary is not needed. `Length`,
  `Percentage`, `Min`, and `Fill` over one axis is a two-pass arithmetic
  function, and it covers most real ratatui layouts. Ship that; say in the doc
  that it is not a solver.
- The widgets. `paragraph`, `list`, `table`, `gauge`, `block`, `tabs` are all
  arithmetic over a `Buffer`.

What does not transfer:

- **`Vec<Box<dyn Widget>>` has no Vyrn equivalent.** Dispatch is static and
  monomorphized, and closures are defunctionalized into closed enums
  (RFC-0037). A heterogeneous widget tree needs a closed enum of widget kinds,
  which is not extensible by a user.

  The answer is to not have widget objects. Make the API immediate in the
  strict sense: a widget is a function that writes into a buffer.

  ```vyrn
  import { Buffer, Rect, splitV, Constraint } from "std/tui"
  import { paragraph, list, block } from "std/tui"

  fn draw(buf: modify Buffer, area: Rect, app: App) {
      let rows = splitV(area, [Length(3), Fill(1), Length(1)])
      block(buf, rows[0], "vyrn")
      list(buf, rows[1], app.items, app.selected)
      paragraph(buf, rows[2], app.status)
  }
  ```

  A user's widget is a user's function with the same signature. Extensible,
  and it needs nothing from the language. It loses ratatui's `StatefulWidget`
  abstraction, which is a fair trade — ratatui's own docs say the state should
  live outside the widget anyway.
- **No `SIGWINCH`.** Poll `hostTermSize` once per frame instead. It is one
  syscall at 60 frames a second and nobody will measure it.
- **`TestBackend`** transfers and is the cheapest correctness win here: render
  into a `Buffer` with no terminal at all and `assertEq` the rows. That makes
  the whole TUI a three-way parity citizen without a terminal, which is what
  makes it verifiable at all.

Cost estimate: rows 1, 3, 4, 5, and 6 of the builtin table, plus the trap hook,
plus roughly the size of `std/html` in Vyrn code. It should not start before
`std/term` has shipped and has users.

---

## Part 3 — Ladder and questions

### 3.1 Gap census

| Gap | Blocks | Severity |
|---|---|---|
| Per-field `///` docs are dropped at parse time | help text from doc comments | High. Worked around by named option types in M1 |
| No environment variable access | `NO_COLOR`, `TERM`, `COLUMNS`, env-backed options | High, and not a CLI-only gap |
| No TTY test | automatic colour and progress degradation | High |
| `print` always ends the line | spinners, in-place bars, prompt lines | High |
| `FnInfo` has no `doc` | a function-per-command shape | Low. This design does not use that shape |
| Enum variants have no docs | subcommand summaries | Medium |
| Shallow `Schema` | nothing here, given named option types | Low |
| No raw mode, no key reads | select, multi-select, fuzzy, password, TUI | Medium. Late milestones |
| No terminal size | help wrapping, bar width | Low. `COLUMNS` covers most of it |
| No signals | resize handling | Low. Poll instead |
| No unwinding, no trap hook | terminal restore after a trap | Medium, and it must be decided before raw mode ships |

### 3.2 Milestone ladder

Each milestone is shippable on its own and has evidence that fails if the
milestone breaks.

**M1 — `std/cli`, one command record.** Flags, options, positionals, unknown-flag
rejection, `--help`, `Validation` errors. No subcommands, no completions, no
colour. Pure generator, zero compiler change.
*Evidence:* `examples/clidemo.vyrn` with a `.args` fixture, a three-way parity
citizen, byte-identical including the exit code on a parse failure; a second
`.args` fixture that fails validation and prints the help;
`vyrn emit-gen` output byte-identical across runs; inline tests over
`parseServe(cliOf([..]))` for every mapping rule; `examples/vlog.vyrn` migrated
from `std/args` to `std/cli` with its existing parity output unchanged.

**M2 — subcommands and completions.** The enum shape; `completionsX(shell)` for
bash, zsh, and fish.
*Evidence:* a two-level subcommand example in the parity corpus; each generated
completion script checked by its shell's own parser in CI (`bash -n`,
`zsh -n`, `fish -n`) — that is the cheapest real check and it catches quoting
faults, which is the whole risk here.

**M3 — field and variant docs in reflection.** `Field.doc`, `EnumVariant.doc`,
`TypeInfo.fields`, `render_type_decl` round-trip. Not a CLI milestone. `std/cli`
switches its help text to it, and `std/http`/`std/graphql` drop their
`lex()`-over-`source` field walks.
*Evidence:* a round-trip test — a record with field docs, reflected and
re-rendered, re-parses to the same declaration with the docs intact; LSP hover
shows a field's doc; `jsonSchema` emits a `description` per field; the two
generators that dropped their scanners produce byte-identical output before and
after.

**M4 — `std/term` styling, plus builtins 1 to 3.** `hostIsTty`, `hostGetEnv`,
`printRaw(fd, s)`. Colour off when not a terminal, off when `NO_COLOR` is set.
*Evidence:* the parity harness pipes stdout, so `hostIsTty` is false and the
three backends stay byte-identical with no special case — that property **is**
the acceptance test; a separate inline test calls the style functions directly
and asserts the escape bytes; the wasm run degrades to the same plain output;
one example prints a coloured error to a real terminal, checked by eye once and
then pinned by the byte test.

**M5 — line prompts and progress.** `confirm`, `input`, `inputValid`; `bar`,
`spinner`. Both hidden when not a terminal.
*Evidence:* a `.stdin` fixture drives the prompts in parity; the same run
produces no progress output at all, proving the hidden target; a test asserts
that a hidden bar accepts every call and writes nothing.

**M6 — raw mode, key reads, selection prompts.** Builtins 5 and 6, and the trap
hook decided first.
*Evidence:* a test that enters raw mode, traps, and shows the terminal restored;
`select` driven by a scripted key sequence through a test-only key source.

**M7 — `std/tui`.** Buffer, diff, layout, the six widgets, `TestBackend`.
*Evidence:* every widget rendered into a `Buffer` and asserted row by row, with
no terminal involved, as a three-way parity citizen.

### 3.3 Open questions

1. **How does a field say "I am the positionals"?** The sketch uses "the one
   `Array<T>` field". A marker alias `type Positional<T> = Array<T>` would be
   clearer, but a generic alias to `Array<T>` is untested — generic type
   declarations exist (`ParamQuery<P, T>` in `std/ui.vyrn`), an alias to a
   built-in container is not known to work. Check before committing to it.
2. **Should short flags be derived at all?** First-byte allocation is
   deterministic but it changes when a field is renamed or reordered, which
   changes a user's command line. picocli and clap both make the author write
   the short name. The alternative here is a named type per short flag, which
   is worse. Consider deriving no short flags and adding them in M3, once a
   field doc can carry the marker.
3. **Is `hostGetEnv` acceptable in the capability model?** It is ambient
   authority. `readFile` is mediated at generation time and free at run time,
   so there is a precedent, but the question deserves its own answer rather
   than arriving inside a terminal RFC.
4. **Does `std/cli` compose with `std/i18n`?** Both are generators. Help text as
   translation keys would need one generator's output to be another's input.
   RFC-0021 does not forbid it. Nothing has tried it.
5. **Where does the exit code come from?** clap exits 2 on a usage error.
   `main` returns `Int64`, so the program chooses. Should `std/cli` export a
   `usageExit` constant so every Vyrn CLI agrees?
6. **Does the trap hook land as its own change?** It is needed by M6, useful to
   any program holding an OS resource, and unrelated to the terminal. It is
   probably a small RFC of its own.
7. **Does `printRaw` take a file descriptor, or does `std/term` get a stderr
   writer?** RFC-0008 owns stderr today and prefixes every line. A CLI needs an
   unprefixed stderr for progress and for error text. This overlaps the logging
   design and should be settled with it, not around it.
