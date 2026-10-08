# Tooling

This page describes the `vyrn` command, the editor support, packages, the
playground and the site build. `vyrn` with no arguments prints the usage screen
and exits 2; `vyrn --version` (or `-V`) prints `vyrn <version>`.

## The file argument

Most commands take an optional `file.vyrn`. Without one, `vyrn` uses the
`"main"` of the nearest `vyrn.json` above the working directory, or exits 2.

Two flags apply to every command that loads a program:

- `--offline` (or `VYRN_OFFLINE=1`): never touch the network. A remote import
  that is not in the lock or the cache is an error.
- `--deny-warnings` (or `VYRN_DENY_WARNINGS=1`): any load warning fails the
  command. Without it, warnings go to stderr and change no exit code and no
  byte of the program's output.

## How a program runs

Every command that executes Vyrn code compiles it to one wasm module first.
`run`, `test`, `bench --check`, `serve` and `dev` run that module in an embedded
wasmtime (Cranelift). `build` without `--target wasm` hands the same module to
wabt's `wasm2c` and compiles the C with clang. There is no interpreter.

A `gen fn` generator also runs as compiled wasm, in the same embedded engine,
while the program loads.

## Commands

### `vyrn run [file] [args...]`

Compiles the file and runs it. Arguments after the file reach the program's
`args()`. The process exits with `main`'s return value; a trap prints one
`error: ...` line on stderr and exits 1.

`vyrn run --profile [file] [args...]` prints to stderr the operations the guest
executed, the blocks it made, freed and held live at its peak, the lines of the
root file that made the most bytes, heaviest first, and the functions that
executed the most operations, with their calls and operations per call. A call
into a function of another file counts at the calling line. The operations are
counted as wasmtime's fuel meter counts them, less the instrument's own: the
sum of the function rows equals the fuel `_start` spends in a plain run
(`VYRN_FUEL=<file>` appends that fuel to a file). `memory.copy` and `memory.fill`
cost one operation per byte, as in the meter. `VYRN_BUILD_PROFILE=1` adds the
compile phases and the `lines read` row. The flag counts only before the file,
so a program can take its own `--profile`.

```
run: 18,495,784 operations; 1,334 blocks, 1,395,504 bytes; 1,334 freed; peak live 335,896 bytes; live at exit 0

line  function         blocks          bytes         live  what
  88  countKmers          153      1,331,584            0  grows tally(..)
  67  thirdSequence       587         22,696            0  enters toUpper(..) in std/strings

function                  calls       operations       per call
runtime$mapSlotI64       68,741        6,277,886             91
countKmers                    7        3,380,670        482,952
runtime$mapPut           33,762        1,688,100             50
```

The run is an audited build with a counter per source line and per function
(`docs/memory.md`). It costs time: on `binarytrees` at order 16 the run took 2.6
times as long as a plain one, on `nbody` at three million steps 1.1 times.
It saves the counts for `vyrn why --cost` under `~/.vyrn/cache/profile`
(`VYRN_PROFILE_DIR` overrides), one file per root.
`VYRN_PROFILE=1` builds the same module for any command that emits wasm, such
as `vyrn build --target wasm`.

### `vyrn check [file]`

Loads the program, runs every generator, type-checks, move-checks and judges
it, and prints `ok`. It also runs the instantiation bound, so a `check` that
passes predicts that `build` terminates. `run` refuses what `check` refuses,
with the same sentence.

Diagnostics print as `file:line:col: message`; `col` is 0 when a stage knows
only the line. Once a file parses, every type and ownership error across all
functions is reported. The parser recovers per declaration and per statement,
and while any parse error exists the later passes are skipped.

`vyrn check --profile` reports the generation phases alone. It needs a cold
generator cache to mean anything.

### `vyrn fix [file]`

Applies the `.copy()` that a diagnostic carries as a `Fix`, at the line and
column the diagnostic gives, in the file given. A `consume p.f` that a hole
refuses becomes `p.f.copy()`: the fix deletes the keyword and inserts the call,
because `consume p.f.copy()` is refused. Every other fix on the menu
(`consume`, `for x in consume xs`) changes an API or a caller's contract, so
`fix` refuses it. A diagnostic with no `Fix` is reported, so is one in an
imported file. A round is kept only if the diagnostic count falls.

### `vyrn build [file] [-o out] [--target wasm]`

With `--target wasm` (or `wasm32-wasi`), writes the module (default
`<stem>.wasm`). It needs no LLVM, clang or sysroot.

Without it, writes a native executable (default `<stem>`, `<stem>.exe` on
Windows). The route is the same module through `wasm2c`, compiled by clang
with the C WASI host (`compiler/vyrn-codegen/src/wasi_host.c`) and wabt's
`wasm-rt`. The intermediate files stay beside the output for inspection:
`<out>.wasm`, `<out>.w2c.c`, `<out>.w2c.h`, `<out>.host.c`.

Every native build passes `-O2 -ffp-contract=off`: contraction into FMA would
change float bits. The microarchitecture comes from a curated set, never a
pass-through `-march`:

| Value | Meaning |
|-------|---------|
| `v1` | `x86-64` baseline, SSE2 only |
| `v2` | `x86-64-v2` (the default) |
| `v3`, `v4` | `x86-64-v3`, `x86-64-v4` |
| `native` | the build machine |

The first of these wins: `--native-target`, `VYRN_NATIVE_TARGET`, the
manifest's `"nativeTarget"`, the default. The setting is inert off x86-64.
`VYRN_DEBUG_SYMBOLS=1` adds `-g`.

Tool discovery: clang through `$CLANG`, then `PATH`, then
`C:\Program Files\LLVM\bin\clang.exe`. `wasm2c` and simde through
`$VYRN_WASM2C` and `$VYRN_SIMDE`, then the `vyrn.lock` pin, then a `tools/`
walk from the source and from the compiler.

### `vyrn test [file] [--name <substring>]`

Runs the root file's `test "name" { ... }` blocks in declaration order and exits
1 if any failed. `assert` and `assertEq` are legal only inside a test body.
Test blocks are type-checked with the program and stripped from `run` and
`build`. A file with no tests prints `no tests` and exits 0.

### `vyrn bench [file] [--name <substring>] [...]`

Runs the root file's `bench "name" { ... }` blocks in declaration order.
`blackBox(x)` is legal only inside a bench or test body.

- Default: times each body in a native harness (the `build` route) and prints
  min, median and mean per iteration.
- `--check`: runs each body once, compiled, with no timing; exits 1 if any
  trapped. This is the deterministic half CI blocks on.
- `--json`: the machine-readable report.
- `--compare <baseline.json> [--threshold <factor>]`: compares each min with
  the baseline's, corrected for the host's speed, and fails only on a bench
  slower by more than the factor.
- `--ungate <file>`: with `--compare`, names benches, one per line (`#` starts
  a comment), whose regressions are reported and do not fail the command.

`--check` excludes `--json` and `--compare`. The harness is imported from
`std/bench` and loaded with the program once, never merged into it.

### `vyrn serve [file] [--port N] [--workers N]`

An HTTP/1.1 host on `std::net` that calls the file's
`fn handle(req: Request) -> Response`. The default port is 8080. `Request` and
`Response` are prelude records, so `handle` is an ordinary function that tests
and `main` can call.

Without `--workers`, requests run one at a time, so module state needs no
locking. A trap inside `handle` is logged and answered with a 500; the server
keeps running on the same instance. The call stack and region nesting return
to their state before the request. Module state keeps what the handler wrote,
and heap blocks it held leak. `--workers N` runs `handle` on N threads, each with its own
instance. Startup refuses it, naming the call path, if `handle` reaches module
state transitively. Printing and file I/O do not block workers; each output
line stays atomic.

### `vyrn dev [--port N] [--workers N]`

The full-stack loop. It needs a `vyrn.json` with `"server"` (the module with
`handle`) and `"client"` (the module built to wasm). It builds the client into
`.vyrn-dev/client.wasm`, then serves the server's `handle` with the `"public"`
directory (default `public`) and the browser runtimes from `web/` in front.

### `vyrn fmt [file ...] [--check]`

The canonical formatter: one style, no options. With no files it formats the
project's `main` and its local imports; if that load fails, it formats `main`
alone and exits 1. `--check` writes nothing, lists the files that would change
and exits 1 if any would.

The printer reads the token stream with comments and chooses only the
whitespace between raw token texts. It sets indentation (4 spaces per brace
depth) and intra-line spacing, drops semicolons, collapses runs of blank lines
to one and ends the file with one newline. It never joins or splits a line and
never rewrites a literal.

The safety invariant: `lex(fmt(src))` equals `lex(src)` with the `;` tokens
removed. `fmt` checks it on every call; a mismatch is an error and the file is
left untouched. The input needs only to lex, not to parse, so format-on-save
works on a half-typed buffer.

`vyrn fmt --from-json <file.json> [--as <Type>] [--from <module>]` prints a
JSON file as VON (Vyrn Object Notation) with a typed header.

### `vyrn doc [file|dir] [-o <dir>] [--std] [--verify]`

Writes Markdown API docs: one `.md` per module plus `index.md`, with each `///`
block verbatim (fenced Mermaid blocks pass through). It documents exports only
and reads a parse, not a checked program. Output is byte-stable: every list is
sorted, newlines are LF. The default output is `docs/api/`; `--std` documents
the standard library.

`--verify` writes nothing and exits 1 if the output directory differs from what
would be generated. CI runs `vyrn doc --std -o ../docs/api --verify`, so a
change to `std/` commits the regenerated `docs/api/`.

### `vyrn why`

Explains a verdict from the source tree. It works on a file that does not
compile, except `--contract`.

- `vyrn why <file>`: the module's audience, the path segment that decided it,
  and every import chain that reaches it.
- `vyrn why --contract <file>`: the module contract that governs the file and
  each export's status against it, as `std/contract` reports it. A `.vyrn`
  module is linked first, as `moduleInterface` links it, so a type reached
  through `import * as m` reads as the generator reads it; a module that does
  not link prints the loader's diagnostics and exits 1, with no report. A
  `.vyx` page is read from its `<script>`, as `vyxPageInterface` reads it. The
  app root is the editor's: the directory of the nearest `vyrn.json`, at any
  depth, else the file's directory. Exits 1 if the file has no role.
- `vyrn why --cost <file>`: per function of the file, the lines that allocate,
  copy, grow a container, enter an allocating function of another file or keep
  a check, with the loop depth of each, and a summary. A copy the compiler
  makes where the program wrote none reads `(implicit)`. A call into a function of
  another file counts that function's allocations at the calling line. A
  function with nothing to report is left out. The LSP's hover keeps the
  per-binding memory rows. After a `vyrn run --profile` of the same source and
  imports, each allocating, copying, growing or entering row also reads
  `last run: N blocks, B bytes`. If the source or an import changed since that
  run, the report prints one line, `profile: stale; ...`, and no counts.
- `vyrn why --capability <fs|stdin|args|extern> <entry-or-artifact>`: every
  import chain that pulls that capability into an artifact's closure.

### `vyrn routes [file] [--json]`

Prints the resolved wire table: every derived, pinned, hand-written and page
path the router mounts, with its source. It reads what generators wrote
(`//@route` lines) and what the program hands `mount(..)`; it never recomputes
a path. `--json` attaches each route's declaration from the generator symbol
maps.

### The `emit-*` commands dump to stdout

- `vyrn emit-wat [file]`: the module `build --target wasm` writes, as WAT.
- `vyrn emit-lowered [file]`: the named core the emitter reads, root module only.
  The text is deterministic and starts with a version line; its format promises
  no stability, and nothing parses it.
- `vyrn emit-gen [file] [--maps]`: the source of every module a generator import
  synthesizes, each under a `// ==== ... ====` banner naming its call site.
  `--maps` prints each module's symbol map as one JSON document per line, with
  the banners on stderr.

### `vyrn new <name>`

Scaffolds `vyrn.json`, `src/main.vyrn` and `.gitignore` under `<name>/`.

## Packages

### `vyrn.json`

The manifest is optional; a single file always runs without one. `vyrn` finds
it by walking up from the file (or the working directory). A manifest that is
not valid JSON, or a key with the wrong shape, is an error, never an empty
policy.

| Key | Holds |
|-----|-------|
| `main` | the entry file for commands given no file |
| `server`, `client`, `public` | `vyrn dev`'s server module, client module and static directory |
| `dependencies` | alias to import specifier; `import { x } from "alias"` |
| `toolchain` | tool name to version: `wasmtime`, `cargo-nextest`, `wabt`, `simde` |
| `nativeTarget` | the default for `--native-target` |
| `audience` | which directory segments are server, client or universal code |
| `artifacts` | named entry plus target (`native`, `wasi`, `browser`); `main`, `server` and `client` are sugar for entries |
| `roles` | directory to module contract, for example `"routes": "std/ui:Page"` |
| `hints` | per-rule severity for the hint libraries (`std/hints`, `std/vyx-hints`) |

An unknown tool name under `toolchain` is refused and the error lists the known
tools. With no `audience` key every module is universal. With no artifacts
there is no capability floor.

### Imports and remote modules

`import { a, b } from "./path"` resolves relative to the importing file, with
`.vyrn` appended. `std/...` is the standard library, found at `$VYRN_STD` or
the `std/` directory near the executable. A remote import is one of:

- `github:owner/repo@ref/path` (a floating ref is resolved to a commit with
  `git ls-remote`; `@ref=<ref>/<path>` names the split when the ref has a `/`),
- `gist:user/id[@rev]/file`,
- `https://...`.

`import type { T } from "x.json"` imports a JSON Schema as Vyrn types.

### `vyrn.lock`, the cache and `vendor`

The first load of a remote fetches it with `curl` and writes a pin to
`vyrn.lock`: one tab-separated line of specifier, immutable URL and sha256. Every later load verifies the sha256. Only `vyrn update` changes a
pin.

- `~/.vyrn/cache/sha256/<hex>`: the per-user module cache, content-addressed.
- `vyrn_vendor/sha256/<hex>`: the project's vendored copies.
- `~/.vyrn/cache/gen`: the generator cache (`VYRN_GEN_CACHE_DIR` overrides it;
  `VYRN_NO_GEN_CACHE` disables it). A generation is keyed by everything it
  observed, absent inputs included, and each entry is authenticated with a
  per-user secret. Never restore it from another machine or CI run.
- `~/.vyrn/tools/<sha>/`: downloaded toolchain archives.

Commands:

- `vyrn add <specifier> [--name alias]`: fetches the module (so a typo fails
  at once), pins it, and adds it to `dependencies`. The alias defaults to the
  file stem.
- `vyrn update [alias]`: re-resolves one dependency or tool, or all of them.
  `--locked` reads through the existing pins and never writes the lock; CI
  acquires its tools this way.
- `vyrn vendor`: copies every locked blob into `vyrn_vendor/`. `--check`
  verifies that each one is there and intact.
- `vyrn deps [artifact]`: prints each declared artifact's module graph, then
  the toolchain rows: each tool's path, version and how it was found (pinned,
  an environment override, or discovered).

A toolchain pin is a `tool:<name>@<version>/<platform>` line in the same lock,
for the four platforms `x86_64-linux`, `aarch64-linux`, `aarch64-macos` and
`x86_64-windows`. Discovery order is the environment override (reported as
one), then the pin, then `PATH` or the `tools/` walk only when nothing is
pinned. A pin that cannot resolve fails; it never falls back to `PATH`. clang
is never pinned, because a native clang links against the host's libc and
linker. `vyrn deps` records its path and version instead.

## Editor support

### The language server

`vyrn-lsp` (`compiler/vyrn-lsp`) is a synchronous server over `lsp-server`,
with no async runtime. It is a pure adapter: it calls the front end's
`analyze_judged` with `vyrn_lower::JUDGE`, the pipeline `vyrn check` runs, once
per change, caches the result and answers every request from the cache, so the
editor and `vyrn check` report the same errors. It
resolves imports through the same loader as the CLI, including manifest aliases
and pinned remotes read from `vyrn_vendor/` or the cache. The editor never
fetches; an unpinned remote gets a diagnostic that says to run `vyrn check`
once.

If the client sends `workspace/didChangeWatchedFiles` with relative patterns,
the server asks for every change under its workspace folders and the std
root. It then reads a file under them again only after an event names it, so
a change the client does not report is seen after the server restarts. A
client without the capability has every module and generator input read on
each change, as `vyrn check` reads them.

It serves diagnostics, hover, go-to-definition (across files), completion
(including `.member` completion from protocol impls and record fields),
document symbols, document highlight, rename (including across a generator
boundary, through the symbol maps), formatting (the same `fmt`), code actions,
inlay hints and semantic tokens. Colour and hover use one resolution order, so
they agree. A `.vyx` file is analysed through the `.vyrn` module that owns it.

Each line that allocates, copies, grows a container, enters an allocating
function of another file or keeps a check ends in an inlay hint with the
verbs and counts of `vyrn why --cost`, such as `copies (implicit), allocates 3`.
The tooltip holds the report's row and the loop depth. A line with none of
these has no hint. The request `vyrn/costLenses` answers one lens per function
that costs anything, such as `allocates 3, grows 1, checks kept 1`.

If the document's last `vyrn run --profile` ran the same source and imports as
the buffer, each hint and lens also reads the blocks that run made there
(`allocates 2`, a middle dot, `171 blocks`). If the buffer differs, the hints show no counts
and one lens reads `profile stale`. The facts come from the analysis that makes
the diagnostics, and exist only while the document has no error. They cost
about 9 ms of a 230 ms keystroke on `site/export.vyrn`. The client's
`costHints` initialization option turns them off, and the server then skips the
work.

The server is outside the Cargo workspace. Build and test it explicitly:

```
cargo build --manifest-path compiler/vyrn-lsp/Cargo.toml
cargo test --manifest-path compiler/vyrn-lsp/Cargo.toml
```

`VYRN_LSP_LOG` names a file for the server's log.

### The VS Code extension

`editor/vscode/` is plain JavaScript with no compile step. `extension.js` starts
`vyrn-lsp` and registers the commands; `vyrn.tmLanguage.json` and
`vyx.tmLanguage.json` colour `.vyrn` and `.vyx` files without the server;
`.von` files have their own language id and reuse the Vyrn grammar. The
grammar's keywords must equal the lexer's; `editor/vscode/test/` checks it.

CodeLenses: Run and Profile over `fn main`; Run test over each `test` block
and Run all tests over the first; the same pair for `bench` blocks; Run dev
server over a root module that imports `rpcServer`. Each runs `vyrn` in a shared
terminal named `vyrn`. The server adds a lens above each function that costs
anything (see above).

Settings:

- `vyrn.serverPath`: the server binary. When empty: `vyrn-lsp` on `PATH` or
  beside the `vyrn` on `PATH`, then `compiler/vyrn-lsp/target/debug/`.
- `vyrn.costHints`: the cost hints and lenses above. On by default. A change
  restarts the server.
- `vyrn.path`: the compiler. When empty: `compiler/target/release/vyrn`, then
  the debug build, then `cargo run -p vyrn-cli`.

For development, build the server, open the repository in VS Code and press
F5. The launch does not rebuild the server, because Windows locks the running
binary. The release workflow publishes one `.vsix` for every platform and ships
`vyrn-lsp` in each per-platform archive.

## The browser runtimes

`web/` holds the JavaScript that hosts a Vyrn module in a page. None of it has
a dependency.

- `wasi-min.js`: a WASI preview1 shim. A module that uses input gets graceful
  degradation: no argv, stdin at EOF, no filesystem, so `readFile` returns its
  canonical `Err`. `runVyrn(bytes, { extern: { ... } })` supplies the `extern fn`
  imports; the declared signatures come from the module's `vyrn:exports`
  section.
- `vyrn-dom.js`: applies the patch stream `std/html` computes in wasm.
- `vyrn-nav.js`: soft navigation between server-rendered pages.
- `vyrn-rpc.js`, `vyrn-query.js`: the client halves of `std/rpc` and page data.

`web/build.ps1` builds the demo modules.

## The playground

`compiler/vyrn-play` compiles the front end and the wasm emitter to a
`wasm32-unknown-unknown` module for the website. It exports `play_tokens`
(highlighting with the real lexer), `play_check` (diagnostics from the real
loader) and `play_compile` (the bytes `vyrn build --target wasm` writes).
`site/public/play-worker.js` runs the compiled program with `web/wasi-min.js`.
A `derive` generator runs in the page: the module calls its one import,
`vyrn_play.run_generator`, and `site/public/play-wasm.js` instantiates the
generator's module synchronously and serves it its `TypeArg` and nothing else.
`std/` is embedded at build time; a relative import reports
`module not found`.

It is outside the workspace, and the build must run from its own directory,
because `.cargo/config.toml` there sets the linker's stack size:

```
cd compiler/vyrn-play
cargo build --release --target wasm32-unknown-unknown
cargo test --release
```

## The site

The website is a Vyrn program. `site/app/` holds the pages and helpers,
`site/app/routes/` the `.vyx` pages, `site/guide/` the guide chapters, and
`site/export.vyrn` the static export. `pages()` from `std/ui` builds the router;
the export calls it once per path and writes each page twice: the document as
`<name>.html` and the soft-navigation payload as `<name>.data.json`.

```
mkdir -p out/docs/std out/guide out/web out/tooling out/explore
python3 scripts/site-history.py > site/data/history.json
python3 scripts/site-demo.py --vyrn compiler/target/release/vyrn > site/data/demo.json
compiler/target/release/vyrn run site/export.vyrn out
node --test "site/test/*.test.mjs"
```

The export cannot create directories or spawn processes, so the caller makes
the directories and the scripts record `history.json` and `demo.json`; the
export refuses to publish without them. `site/release.txt` holds the newest
published tag. The export checks that every internal link resolves, and the
node tests serve `out/` under a path prefix and fetch every page and link.

`site.yml` runs this on every pull request, plus `vyrn test` over each site
module, `vyrn run site/markup.vyrn` (the template hints) and `vyrn fmt --check`.
On `main` it publishes `out/` to GitHub Pages.
