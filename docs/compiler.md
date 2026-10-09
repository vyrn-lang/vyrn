# How the compiler works

The compiler reads Vyrn source and writes one WebAssembly module. Every way a
program runs starts from that module. A rule is stated in one place: a desugar
into a small named core, a judgment a kernel makes over that core, or a table
the runtime reads. The emitter reads the core and writes wasm.

This page is for a person who changes the compiler. `docs/memory.md` describes
the ownership model the kernel enforces. `AGENTS.md` holds the process and the
exact gate commands.

## The pipeline

```
source
  -> lexer, parser                    vyrn-frontend  lexer.rs, parser.rs
  -> loader: imports, generators,     vyrn-frontend  loader.rs, gen.rs
     link into one Program                            (+ vyrn-genwasm)
  -> checker: types, predicates       vyrn-frontend  checker.rs
  -> synthesis: `where` constructors  vyrn-frontend  ctor.rs
  -> lowering: instances, named core  vyrn-lower     lib.rs, core.rs
  -> kernel: linear, effect and       vyrn-lower     kernel.rs, effects.rs,
     typed judgments; the placer                      typed.rs
  -> floor: what the target reaches   vyrn-frontend  floor.rs
  -> emitter: one wasm module         vyrn-codegen   direct.rs, wasm.rs
  -> run: wasmtime in process, a      vyrn-cli       wasmrun.rs, main.rs
     .wasm file, wasm2c + clang,
     or a browser
```

Everything up to the floor runs on every command that loads a program:
`check`, `run`, `build`, `test`, `bench`, `serve`, the language server and the
playground. `vyrn check` stops there, after one more question the emitter
would otherwise answer late: `vyrn_codegen::check_instantiations` refuses a
polymorphic recursion that has no finite set of instances.

## The crates

The workspace members build and test with no LLVM, no clang and no wasi
sysroot. A crate is judged on what it costs, not on a rule against crates.

| Crate | Owns | Depends on |
|---|---|---|
| `vyrn-frontend` | lexer, parser, AST, loader, checker, the ownership vocabulary, the core's data types (`core`), the editor queries, the formatter, the manifest and lock reader | nothing |
| `vyrn-lower` | the lowered form, the builder of the named core, the placer, the three judgments | `vyrn-frontend` |
| `vyrn-codegen` | the wasm emitter, layout, the module encoder, the toolchain finder, the WASI host in C for the native route | `vyrn-frontend`, `vyrn-lower`, `wasm-encoder`, `wasmprinter` |
| `vyrn-cli` | the `vyrn` driver and the in-process WASI host | all of the above, `vyrn-genwasm`, `wasmtime` |
| `vyrn-genwasm` | runs a `gen fn` as compiled wasm inside a load; without its `host` feature, builds the module a host runs (`run_pure`) | `vyrn-frontend`, `vyrn-codegen`, `wasmtime` (feature `host`) |
| `vyrn-lsp` | the language server, an adapter over `vyrn-frontend` | `vyrn-frontend`, `vyrn-lower`, `vyrn-genwasm` |
| `vyrn-play` | the playground: the front end and the emitter compiled to `wasm32-unknown-unknown` | `vyrn-frontend`, `vyrn-codegen`, `vyrn-lower`, `vyrn-genwasm` without `host` |

`vyrn-lsp`, `vyrn-genwasm` and `vyrn-play` are excluded from the workspace.
Test and format the first two with `--manifest-path`. Build `vyrn-play` from
its own directory, so its `.cargo/config.toml` sets the linker stack size.

The dependency edge from `vyrn-lower` down to `vyrn-frontend` is one way. The
front end cannot call the lowering. A host enters through `vyrn-lower`
(`load`, `check_and_synthesize`, `analyze`), which calls the front end and
then its own judgments. The editor passes the pipeline after the load into
`symbols::analyze_judged` as a value (`vyrn_lower::JUDGE`). A host passes the
generation engine the same way, beside the resolver of each load and check.
`vyrn_genwasm::engine` is the wasmtime engine; the playground builds its own,
which runs the module in the page. Both wrap their run in
`vyrn_lower::gen_engine`, which judges the generator's own program first. The
engine rides in `gen::GenInputs`, so a generator's own loads and `derive`
sites run on it too. A load with no engine fails every `derive` and generator
import.

## The front end

`lexer::scan` reads the source once; `lexer::lex` and the formatter read the
scan. `parser::parse` is recursive descent with precedence climbing. It
recovers past a bad top-level declaration, so one file reports every parse
error. The parser performs the pure syntax desugars: `??` into a `match` over
`Pattern::Success` and `Pattern::Failure`, a refutable `let` into a `match`
with `Pattern::Other`, `if let` and `while let` into a statement `match` with
`Pattern::Other` (`Expr::as_if_let`), interpolation into a `@concat` chain. Source cannot
spell these patterns or `@` names.

`loader::load_with_origins` builds one `ast::Program` from a root file. All
I/O goes through a `ModuleResolver`, so the editor serves unsaved buffers and
the playground serves an embedded `std/`. The loader:

- resolves every `import` transitively, from disk, `std/`, the lock and cache
  (`manifest.rs`), or a generator call;
- runs each generator call (`run_generator`) through the load's engine,
  caches its output under its recorded inputs, and maps generated lines back
  to their origin (`origin.rs`);
- links the modules (`link`), so no later pass sees a module boundary, and
  refuses cycles, missing or unexported names, duplicates and two impls of
  one pair;
- links `std/runtime` into every program, each declaration renamed with the
  `runtime$` prefix, and links `std/json` and similar modules when a builtin
  that lives in one is mentioned;
- enforces the fences: only `std/runtime` may import `std/mem`, and nothing
  may import `std/runtime` (`runtime_fence`). The audience (`audience.rs`)
  and the floor (`floor.rs`) fence the rest.

A thread's later loads reuse what one text alone decides: a module's parse and
the names it references, keyed by its text, and each generator cache entry the
load read, validated against its inputs again. So an editor keystroke parses
only the edited text. A host that is told of every file change keeps, in its
`session::Session`, each file's text, each directory's listing and each
canonical path under the directories it watches (`Session::watch`), until
`Session::changed` names the path. Any other host reads every module and
generator input on every load. Every load links the whole program again.

`vyrn_lower::check_and_synthesize` then runs, in order, the frontend's
`check_and_synthesize` (steps 1 and 2) and the judgments in `vyrn-lower`:

1. `checker::check_accum_with_sites`: names, types, calls, `mut`,
   all-paths return, and each `where` predicate against constant arguments
   (`consteval.rs`). The checker records every expression's type in
   `checker::Recorded`, keyed by node address. Every later pass reads types
   from this record and derives none of its own. The bodies a `derive` or the
   synthesis appends add their types to it (`Recorded::extend`), so the
   judgments read the one record this check made.
   If the program calls `derive(g, x)`, `gen::derive` runs each generator
   once over a `TypeArg` of the types its sites need, and the functions it
   writes join the program with the type declarations it writes and the
   `where` constructors. Each site's entry must have the signature its call
   needs. `checker::check_appended` types their bodies
   against the joined program's declarations, and a whole check runs only in
   the case its doc names. `toJson(x)` is a `derive` site of `std/json`'s
   `jsonEncoders`, and `fromJson<T>(s)` of `std/jsondec`'s `jsonDecoders`.
2. Synthesis, only for a program that type-checks: `ctor::constructors`
   generates one constructor per `where` type the `derive` join did not add.
   These are ordinary functions, so every backend compiles one body.
3. `vyrn_lower::refusals`: `vyrn_lower::analyze`, which runs the placer and
   the judgments, then one list of ownership refusals in source order.
   `analyze` returns the World (`vyrn_lower::World`): the `Ownership` with
   the checker's record, the function table, the call relation
   (`World::callees` and `World::callers`), the read relation
   (`World::readers`: the declarations and misses each source body's name
   lookups read, when the host armed `checker::record_reads`), the core's bodies and facts,
   and both refusal lists. The plan's rows, the bodies and the effect
   judgment's state rows are keyed by `FnId`; a frame carries its row
   (`Body::id`), which the placer's serial merge gives a lambda frame. A
   reader turns a name into an id once, with `World::fn_id`. The
   emitter reads the same World: `vyrn_lower::load_warned` returns it with
   the program, and a host that changes the program analyses it again.
4. `floor::decide`: whether each artifact's target provides what its code
   reaches.

For a program that does not type-check, `lower_typed` still builds every
function the type errors do not reach and adds the typed judgment's refusals,
so one run reports both kinds. A generator's own program gets steps 1 and 2.
The judgments run in its engine's compile (`direct::compile_gen_host`), which
refuses the program the typed judgment refused, or else the program with a
kernel must-use row, before it emits. The engine (`vyrn_lower::gen_engine`)
refuses a program the run declines with its must-use rows, so the program is
refused whether or not the engine serves it. The kernel's other refusals are
not printed. A refused compile yields no module, so no cache holds one, and a
warm cache reports the same refusals.

The editor runs the same pipeline. `symbols::analyze_judged` loads the
document as `vyrn_lower::load` does, an untitled buffer as `untitled.vyrn` in
the working directory, and hands the linked program and the load's pending
floor decision to the `Judge` it is given (`vyrn_lower::JUDGE`: the steps
above). Around it the editor keeps what only it needs: the parser's recovery,
so a partial program is still indexed; in the `session::Session` its server
owns, the per-body judgment memo, the per-body recheck (`checker::recheck`),
which types a body again only when its text or one of its reads' answers
changed, and the lowering's walk of each body the recheck holds, which the
placer's worklist reuses; the diagnostics' columns; and the memory rows the `Judge` copies off the World. `symbols::analyze` and `analyze_linked` run the checker alone. Each
returns diagnostics with columns, the symbol index and the tokens. `vyrn-lsp`
serves hover, definition, completion, references and rename from that
`Analysis`, and holds no rule of its own.

## The lowered form and the named core

`vyrn_lower::lower` and `lower_with` build a `Lowered`: one `Instance` per
instantiation, with no type parameter left, keyed by its type arguments
(`vyrn_lower::spell` gives `map<Int64, String>`). `NodeTypes` holds the
checker's type for each expression, substituted through the instance. The
walk expands what every engine must see the same way: a projection inlined at
its access site (`project::site`), an optional projection under `if let`,
`a[i] = v`, and a `for` over a user container. The worklist runs in waves:
each wave walks every body the last one found, on every thread, and follows
their calls in queue order, so the instances and the unresolved calls come out
as on one thread. The walks read the expansions typing made and make none;
they run under `Expansions::seal`.

`core::build` lowers each instance into the named core. Every intermediate
value has a name, every access is a place, and control flow stays
structured. The statements (`core::St`) are `Let`, `Store`, `Drop`, `Row`,
`If`, `Loop`, `Block`, `Break`, `Continue`, `Return`, `Switch`, `Do`,
`Trap` and `Check`. A `Check` row (`vyrn_lower::check`) states one runtime
check of the row after it: its trap rule, what it compares, and its line and
ordinal. `core::checked` adds them to the bodies the emitter reads, and the
emitter runs a check only from its row. `check::mode` reads `VYRN_CHECKS`:
`keep` keeps every row, and a file path is the oracle, which counts each row's
runs into that file and fails a run where a proved row would have trapped. The
count is in the module: each row is a counter row of the site table
(`vyrn:sites`), as the profile's sites are, so `vyrn_check.fail` is the oracle's
only host import.
`scripts/check-elision.sh` runs the examples, the benchmarks and the site
export in all three modes. `elide::decide` marks a row proved when linear
facts over one body's own names (`facts`) show it cannot fail, with a
certificate `facts::Cert::verify` checks again. A fact may name the length of
a record name's array field, and a `where` rule's `a.length == b.length` makes
one term of both (`facts::Term::Col`), except inside a group of stores into
the record's fields, where each field has its own term until the row that
checks the rule (`check::Guard::Rule`); `World::body_of` decides a body
when an emitter first reads it. A direct call's result takes the facts every
return of its callee proves (`elide::summaries`, keyed by `FnId`, solved once
per World by `fixpoint::descend`); a call through a value, a method, a
projection or a generic instance takes none. A private function that only
direct call rows enter starts with the facts every such row proves of its
arguments (`World::summaries` lists the candidates, `elide::entered` drops a
function any row spells by name). The same walk answers a group's rule check
that the facts prove false (`elide::refuted`); `vyrn check` states and walks
its own copy of a body with a group for it, and `typed::groups` refuses each
such group at its first store. A right-hand side (`core::Rhs`) is a value, a `Read` or `Take` of a
place, a `Call`, a `Prim` (one row of the primitive table), a `Make` of a
record, array or variant, or a function name. A place (`core::Place`) is a
name, a global, a field, an element or a map key. Evaluation order is left
to right, and the naming fixes it.

`core::build_module_state` lowers the module-state initializers
and `core::build_outside` a `test` or `bench` body. A module-state initializer or a
`where` predicate is also built for the typed judgment alone. `core::builtin_rows` collects the
`prelude::Builtin` rows the core states as calls, `Prim` rows or rebuilds (`core::Spec`).

A construct `core::build` cannot state returns a `Gap`. The instance is then
unlowered: the kernel reports "internal error: the core cannot state ..." and
the emitter refuses the body. A gap is a compiler defect.

`vyrn emit-lowered <file>` prints the core of the root module. Its format
carries a version line (`vyrn_lower::VERSION`) and promises no stability;
`tests/lowered_dump.rs` pins a few snapshots.

## The kernel: three judgments over the core

The kernel knows no surface syntax. It judges core bodies.

- Linear (`kernel.rs`). Every owned name is consumed exactly once on every
  path from its binding. `kernel::placement` walks a body in placement mode
  and reports the releases it is missing; `core::augment` turns them into
  release rows and rebuilds the body with a `St::Drop` at each. What no
  placement repairs (a second consume, a use after a release, an escaping
  borrow, a write under an alias) is a refusal. `docs/memory.md` states the
  rules.
- Effect (`vyrn_lower::effects`). A body's effect set is the join of its own
  atoms and its callees' sets, taken to a fixpoint by `fixpoint::solve`,
  callees first. The lattice is `vyrn_frontend::effects::Effect`, fourteen
  effects: `alloc`, the I/O effects, `extern`, `serve`, module state, `trap`
  and generation-only. The floor asks it what an artifact reaches
  (`effects::reaches`), and the kernel asks it which globals a call may
  write (`effects::writes_state`), because such a call ends every borrow of
  those globals, its own arguments' too.
- Typed (`typed.rs`). The `vyrn check` rules over the core rows: a store
  into a place not declared `mut`, a store group that leaves a `where` rule
  unchecked, an exit outside a loop, a `drop` that releases nothing, and the
  rules the builder met at its construct. A value of a validated type is
  produced only by that type's constructor, a name already of that type, or
  a literal the checker proved; the census `tests/typed.rs` walks each store
  into a validated place and judges its producer. `vyrn_frontend::validate`
  says which types carry a rule; a sized integer is judged by width and
  signedness.

`movecheck.rs` states no rule. It orders refusals by source
(`movecheck::in_source_order`) and memoizes per-body judgments for the editor (`movecheck::Judgments`), so a
keystroke re-judges only the bodies whose key changed.

## The emitter

`vyrn_codegen::direct::compile` writes a self-contained `wasm32-wasi` module:
its own memory, heap and runtime, importing only the WASI calls it makes and
one `vyrn` import per `extern fn` (a host-boundary name such as the clock is
served from WASI instead). `direct::wat` prints the same module as
text (`vyrn emit-wat`). `direct::compile_gen_host` compiles a generator: the
same module plus the `vyrn_gen` imports, and `Code` as an `i64` handle.

The emitter walks each body from the core. `lower_body` fetches the body the
core built under the instance's key (`World::body_of`), and `Fn_::core_walkable`
screens it; a body the screen rejects is refused with "the body of `f` the
core did not state". The emitter places no release and derives no type: a
`St::Drop` becomes a call, and an expression's type comes from the checker's
record (`core::node_ty`).

The emitter carries no optimizer. Optimization belongs to whatever runs the
wasm. The emitter's job is to make that possible: a field read is one scalar
load at a computed offset, never a copy of the aggregate
(`tests/fieldstore.rs` counts the instructions).

The module's shape:

- Memory (`wasm.rs`): a shadow stack of `STACK_BYTES` growing down from
  `STACK_TOP`, then data from `DATA_BASE` up to `STATICS_LIMIT`, then the
  heap from the address in the global `HEAP_BASE`, 16-aligned past the
  statics. A frame push past address 0 wraps and traps on first
  access instead of overwriting data.
- Values: a scalar lives in a wasm local; an aggregate lives in a frame slot
  and travels as its `i32` address; an aggregate result goes through a hidden
  leading address. A `read` or `modify` parameter is used at the caller's
  address when no module state is an aggregate or owns heap other than a
  `String`'s (`Cx::args_in_place`). Otherwise the callee copies it in at
  entry, and a `modify` one back out at the one exit. A `consume` parameter
  is copied in, unless every `return` yields it (`Sig::in_place`): then it
  is the result, the call has no out-pointer, and the caller moves the
  argument into the destination. It passes `x`'s storage instead for
  `x = f(x, ..)`, and for an argument whose extent ends at the call and that
  holds a frame slot, which the result then takes.
- Layout (`layout.rs`): `shape_of` maps a type to a `Shape` of `Leaf`s, and
  sizes, alignments, offsets, loads, stores and the call ABI are all read off
  it. Equal shapes are one representation. Every size is a checked `u32`.
- Control flow: `St::If`, `St::Loop`, `St::Block` and `St::Switch` map onto
  wasm's `if`, `block` and `loop`, and `break` and `continue` onto `br`. A body never emits `return`, which would skip the frame's
  epilogue; a `return` is a `br` to the body's outer block.
- Traps: a failed check stores its row and value and branches to the one
  `trapAt` call per function. The wording is `vyrn_frontend::trap`'s table,
  laid out as data; `std/runtime` prints it. `tests/traps.rs` fails if any
  other source file spells a wording.
- Boundaries: `coerce_plan` picks the rung a value takes into a declared type
  (`Rung`): validate, resize, cross the float line, rebuild a record, reshape
  a sum, and the rest. `Rung::Validate` runs the type's `where` predicate.
- `Module::sweep` drops every import and function the program never reaches,
  writes no byte of a datum it never reaches, and ends the static area at the
  last live datum or reservation. Addresses do not move, so a dead datum below
  that end keeps its address space.

`std/runtime.vyrn` is the runtime: allocator, arena, strings, arrays, maps,
UTF-8, number formatting, file and console I/O, and the trap printer. It is
ordinary Vyrn over the primitives of `std/mem`, whose functions have no body:
the emitter lowers each call to one wasm instruction or one host import
(`mem_ins`). The map value paths (`map_set`, `map_tally`, `map_at`)
stay emitted in `direct.rs`: under wasmtime one extra call level costs 14%
on k-nucleotide, and their per-type value steps have no Vyrn home. They move
when the wasm route inlines runtime leaf calls.

## Where the module runs

| Route | Command | How |
|---|---|---|
| In process | `vyrn run`, `test`, `bench --check`, `serve` | the embedded `wasmtime` under `vyrn_cli::wasmrun`, a hand-written host for the WASI calls `direct.rs` declares |
| A file | `vyrn build --target wasm` | the bytes as emitted; `wasmtime run --dir .` runs them |
| Native | `vyrn build`, `vyrn bench` | `build_wasm2c`: wasm2c turns the module into C, and clang compiles it with `wasi_host.c` and wabt's `wasm-rt` |
| Browser | `web/`, the site's playground | `web/wasi-min.js` answers the same imports with a browser's limits: no argv, EOF on stdin, no files |

`vyrn_codegen::toolchain` finds wasm2c, clang, simde and wasmtime. `vyrn.lock`
pins wabt, simde and wasmtime by sha256 under `tool:` rows, unpacked under
`~/.vyrn/tools/`. clang is recorded, not pinned. `--native-target` picks the
x86-64 level (the default is `v2`). clang always runs with
`-ffp-contract=off`, so a native float result equals the wasm result.

`vyrn test` and `vyrn bench --check` compile the selected bodies into one
module, each an exported `__vyrn_body_<k>`, and run them in one instance, so
a body sees the module state the one before it wrote. A trap ends its body,
prints the `FAILED:` line, and the next body runs.

A `gen fn` runs inside the load. `vyrn-genwasm` compiles the generator's
module with a synthesized `main` that dispatches on its first argument, runs
it in wasmtime, and reads the result between two sentinel lines. File reads
go through the loader's resolver and are recorded as the generator's inputs;
`lex`, `moduleInterface` and `contractOf` cross as a host encoding and a
synthesized decoder. A generator the engine declines is an error: "the
installed generation engine declined it".

## What each gate proves

`AGENTS.md` lists the commands and when to run them. This section says what
each one is evidence of. The tests live in `compiler/vyrn-cli/tests/`.

The output of programs:

- `fixtures.rs`: every example, run as compiled wasm in the embedded engine,
  prints its recorded `examples/expected/<name>.stdout`, `.stderr` and
  `.exit`. It proves the output has not moved since a person reviewed it.
- `route.rs`: every corpus program built natively prints the same bytes and
  exits with the same code as its wasm under the `wasmtime` CLI. It needs
  clang and the pinned tools.
- `wasmhash.rs`: the SHA-256 of every example's wasm against the manifest
  `compiler/vyrn-cli/tests/pins/wasm-sha256.tsv`. CI runs it on every platform with
  `VYRN_WASM_MANIFEST=check`, so output that depends on the host (a hash map's
  order, a platform `usize`, a path in the bytes) fails on the leg where it
  differs. `VYRN_WASM_MANIFEST=write` rewrites the manifest; read every moved
  row with `wasm2wat` before committing it.
- `reproducible.rs`: one source builds to one artifact in every process.

The refusals:

- `refusals.rs`: one minimal program per ownership refusal in
  `tests/refusals/`, with its whole sentence and the pass that states it. A
  row runs twice, with and without the kernel (`VYRN_NO_KERNEL=1`), to
  attribute the sentence.
- `check.rs`: every `tests/check/<name>.vyrn` against `<name>.stderr`, the
  exact output of `vyrn check`. No `.stderr` means accepted. Add a test as the
  two files; `VYRN_PIN=write` writes the `.stderr`.
- `scripts/check-corpus.sh <tree> <out>` runs `vyrn check` over every `.vyrn`
  file in `examples`, `std`, `site` and the test directories, and writes each
  file's stdout, stderr and exit code. `diff -r` of main's output against the
  branch's is the refusal corpus: every refused program stays refused, on the
  same line, with the same sentence.
- `testsweep.rs`: every Vyrn program written inside a test's string literals,
  checked with and without the kernel. A program only the kernel refuses is a
  finding.
- `columns.rs`: every refusal the editor shows sits on a real token.

The core and the judgments, over the whole corpus. These are `#[ignore]`d and
run as their own gate:

- `kernel.rs`: every instance, initializer, `test`, `bench` and lambda frame
  lowers to the core and passes the linear judgment.
- `coredrive.rs`: the emitter takes every body of the corpus and of
  `tests/shapes/` from the core's rows. A shape is a small program for a case
  the examples miss; add one as a new file.
- `coretables.rs`: a census of the core's side tables; every `Rhs` names the
  type its node produces.
- `effects.rs`: the effect judgment agrees with the floor and the audience
  pass, or the disagreement is on the ratchet.
- `typed.rs`: every store into a validated place has a legal producer.

Memory at run time:

- `residue.rs`: every corpus program under the free audit (`VYRN_LEAK_CHECK=1`)
  on both routes, against `compiler/vyrn-cli/tests/pins/residue-baseline.tsv`. The baseline
  only shrinks.
- `memory.rs`: a long-lived instance's memory after N calls equals its memory
  after 4N; selected shapes under the audit.

The size of the compiler. A census tiles a file into sections, each an anchor
item with its doc comment, so the counts add to the file's length. Each
section has a kind and a count of diagnostic sites, so a rule that leaves a
file moves both numbers. The pins are in `tests/pins/`:

| Test | Counts |
|---|---|
| `checker_census.rs` | `checker.rs` by section, and its rules (`checker-census.tsv`, `checker-rules.tsv`) |
| `emitter_census.rs` | `direct.rs` by section, and what each class reads (`emitter-census.tsv`, `emitter-reads.tsv`) |
| `frontend_census.rs` | `loader.rs`, `symbols.rs`, `project.rs`, `movecheck.rs` (`frontend-census.tsv`) |
| `parser_census.rs` | `parser.rs` and `lexer.rs` (`parser-census.tsv`) |
| `cli_census.rs` | the CLI crate, by kind and by command (`cli-census.tsv`, `cli-commands.tsv`) |
| `forms.rs` | every statement, expression and pattern form, declaration and keyword, priced in compiler lines (`forms.tsv`, `keywords.tsv`, `contextual.tsv`, `declarations.tsv`) |
| `surface.rs` | every `ast::Type` constructor and its cost (`surface.tsv`) |

A pin is data a gate writes. Rewrite it with `VYRN_PIN=write`, read every
line that moved, and commit it with the change that moved it. Never merge a
pin file by hand; `.gitattributes` marks them `merge=pin`.

The smaller gates: `lowered.rs` (the emitter's type at each expression equals
the lowering's), `traps.rs` (one home for trap wordings), `limits.rs` (each
resource limit refuses with a named cause), `std_suite.rs` (every `test` block
in `std/` runs), `vyrn doc --std --verify` (the API docs match `std/`), and
`fmt.rs` with `cargo fmt --check` (the formatter's re-lex invariant and the
Rust style).

## Switches for diagnosis

| Variable | Effect |
|---|---|
| `VYRN_NO_KERNEL=1` | stands the kernel aside, to attribute a refusal |
| `VYRN_KERNEL_TRACE=1` | prints each release the placer adds or cannot place |
| `VYRN_THREADS=<n>` | types, walks, builds and places bodies on `n` threads; `1` keeps a trace in body order |
| `VYRN_SHUFFLE=<seed>` | permutes the order the checker's, the lowering's and the placer's threads take bodies in |
| `VYRN_LEAK_CHECK=1` | builds with the free audit |
| `VYRN_FUEL=<file>` | meters `vyrn run` and appends the fuel `_start` spent, tab-separated from the program's name |
| `VYRN_WASM_NAMES=1` | writes function names into the module |
| `VYRN_GENWASM_TRACE=1` | prints the generation engine's phase timings |
| `VYRN_TYPED_DUMP=<file>:<fn>` | prints one body's judged stores (`typed.rs`) |
| `VYRN_EFFECTS_DUMP=<file>:<fn>` | prints one function's effects and callees (`effects.rs`) |
| `VYRN_PIN=write` | rewrites the census pins |
| `VYRN_WASM_MANIFEST=check` or `write` | compares or rewrites the wasm manifest |
| `VYRN_BLESS=1` | re-blesses the `emit-lowered` snapshots |

`vyrn check --profile` and `VYRN_BUILD_PROFILE=1` print where a load or a run
spent its time, per phase (`prof.rs`). `vyrn run --profile` prints the guest's
operation count and the blocks it made per source line (`direct.rs` `Profile`,
`std/runtime` `auditBirth`, `wasmrun.rs` `Counts`).

`vyrn why --cost` prints `insight::facts` (`insight.rs`): per line, the rows of
the decided core that allocate, copy, grow a container or keep a check. A
callee outside the root file counts at the calling line through
`World::allocates`, the effect judgment's `alloc`. A kept check carries its
reason in `Check::why`, written beside the verdict by `elide::Walk::why` from
the first goal that failed and the origin of its names (`elide::Origin`); it
calls the prover no more often.

## Where a change goes

- A new surface form: a desugar in the parser, or a lowering in `core::build`.
  The kernel and the emitter learn nothing new if the form lowers into
  existing statements.
- A new ownership rule: `kernel.rs`, with a program in `tests/refusals/` and a
  row in `refusals.rs`. The checker states no ownership rule.
- A new refusal or acceptance test: a program in `tests/check/` and its
  `.stderr`, never a program in a Rust string.
- A new builtin: one `prelude::Builtin` row, with its contract (a capability
  per parameter), method spelling, `Spec`, effect, route, length effect and
  editor text; its operand count (`takes`) and, when no `sig` types it, its
  operand and result types (`typed`); `fresh` when the result shares no
  storage with an operand. A row whose typing needs a receiver's kind or a
  name keeps one arm in the checker. Its body is in `std/runtime` when it can
  be Vyrn, otherwise a lowering in `direct.rs`.
- A new trap wording: `vyrn_frontend::trap`, and nowhere else.
- A new effect: the builtin row's `effect`, or `effects::RUNTIME_ATOMS` for a
  runtime function, then the floor's table.
- A change to emitted bytes: rewrite the wasm manifest and explain every
  moved row.
