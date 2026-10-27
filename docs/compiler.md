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
- parses an imported `std/` module's top-level function bodies only where a
  root can reach them by spelling (`parse_reached`), and drops each function,
  test and bench of such a module that no root reaches by name
  (`drop_unreached`), so an importer pays to lex the module and to parse and
  check what it calls. The import of a generator an import line calls reaches
  nothing, because the importer only runs it. `std/runtime` is such a module, and its roots include
  every function the emitter calls by name (`loader::RUNTIME_CALLS`, the one
  table both read) and every function a reached builtin routes to; a
  generator's own load keeps it whole, because its engine adds calls after the
  load (`lazy_runtime`);
- links `std/runtime` into every program, each declaration renamed with the
  `runtime$` prefix, `std/task` with the `task$` prefix into a program that
  imports it, and `std/json` and similar modules when a builtin that lives in
  one is mentioned;
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
certificate `facts::Cert::verify` checks again, or, for a divisor, a
disequality `d != 0` the state holds. A fact may name the length of
a record name's array field, and a `where` rule's `a.length == b.length` makes
one term of both (`facts::Term::Col`), except inside a group of stores into
the record's fields, where each field has its own term until the row that
checks the rule (`check::Guard::Rule`). A name of a validated integer type
carries its rule's comparisons (`elide::Walk::premises`), and a construction of
one from a name is a check row (`check::Guard::Scalar`). A record fact
(`elide::RecordFacts`) is a linear fact over the fields of every record
value with those fields: two array fields equally long, an integer field at
least zero, an indexed array or String field at least as long as an integer
field's value or longer, or at least a constant long where a check needs it.
A walk that proves a record's facts takes a callee's constant result length from a solve
that assumes no record fact. `elide::invariants` keeps a fact only when each body that
builds such a record or writes one of its fields proves it wherever the
value leaves the name, and a walk assumes it of every record name it did not
build. An element fact bounds every element of an integer array field: at
least zero, and below the length of a field of the record by one, or by the
reach of a column a check adds to the element (`elide::reach`: `s + x`, `x`
a narrow unsigned value or a mask). It holds
when each store into an element and each push onto the field proves the
bound where it runs, a record literal's field is empty or a copy of another
record's with a bounding field at least as long, no row shortens the
bounding field, and no row changes the elements where no walk sees it
(`elide::element_scan`); a read of an element assumes it. A fact may name the value of a
record name's integer field (`facts::Term::Field`): by the kernel's
exclusivity judgment only a row that names the record writes the field, so
each read between two such rows is one value, and a store defines it. A
check on a global's array field reads the length of a borrow of the global
while the state holds the borrow fresh (`facts::State::fresh`); a row the
effect judgment says may write the global ends that (`elide::Walk::stale`).
`World::body_of` decides a body
when an emitter first reads it. A direct call's result takes the facts every
return of its callee proves (`elide::summaries`, keyed by `FnId`, solved once
per World by `fixpoint::descend`); a call through a value, a method, a
projection or a generic instance takes none. A result exactly as long as a
`consume` parameter takes the argument's length as it was before the call
(`elide::Walk::same_length`). For an `Option` of an integer
the facts are its payload's, and for an `Option` of a record its payload's
integer fields' (`elide::payload_fields`), over every `Some` return (a `None`
proves them all), and the binder of a `Some` arm takes them
(`elide::some_binder`). A private function that only
direct call rows enter starts with the facts every such row proves of its
arguments (`World::summaries` lists the candidates, `elide::entered` drops a
function any row spells by name). A refusal reads the demanded walk
(`elide::demanded`): the same walk over the body, its parameters' clauses, its
names' validated types and the declared result clauses of its direct callees
(`elide::Summaries::declared`), and no inferred summary or entry fact, which
the judgment memo does not key. The typed judgment states and walks its own
copy of a body with a group or a `prove` call by it. `typed::groups` refuses
a group whose rule check the facts prove false at its first store, and a
`prove` row the walk keeps (`check::Raises::Prove`, a clause check of the
comparison the call's argument computes) is refused with its `Check::why`. In
a module the manifest's `proved` key names (`Program::proved`, from
`manifest::Proved`) every kept row is refused, and `World::body_at` decides
the module's bodies by the same walk, so the emitter and `vyrn why --cost`
read the verdicts the refusal read. A right-hand side (`core::Rhs`) is a value, a `Read` or `Take` of a
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

`vyrn emit-lowered <file>` prints the core of the root module: the bodies the
emitter reads (`World::body_of`), never a second build. Its format carries a
version line (`vyrn_lower::VERSION`) and promises no stability;
`tests/lowered_dump.rs` pins a few snapshots. `tests/verdicts.rs` holds its
check verdicts equal to those of every example's oracle build.

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
  callees first. The lattice is `vyrn_frontend::effects::Effect`, sixteen
  effects: `alloc`, the I/O effects, `extern`, `serve`, module state, `trap`,
  generation-only, `waits` and `net`. An atom of `net` also waits
  (`Effect::set`). The floor asks it what an artifact reaches
  (`effects::reaches`), and the kernel asks it which globals a call may
  write (`effects::writes_state`), because such a call ends every borrow of
  those globals, its own arguments' too. A frame whose own atoms wait may
  write every global, so each waiting call ends every borrow of module state
  and every fact about it. The judgment states the waiting rules
  (`effects::Judged::wait_rules`) over every frame, served ones and each
  module-state initializer, which no call enters, included: a waiting call
  is marked `await`, a marked call waits, and an initializer waits nowhere.
  The builder refuses a mark on a call it states as no call row
  (`core::Builder::awaits_nothing`). An awaited call row carries its
  cancellation edge (`core::Cancel`): the releases the placer puts at
  `own::Exit::Cancel`, which the kernel judges like a `return`.
- Typed (`typed.rs`). The `vyrn check` rules over the core rows: a store
  into a place not declared `mut`, a store group that leaves a `where` rule
  unchecked, an exit outside a loop, a `drop` that releases nothing, and the
  rules the builder met at its construct. A value of a validated type is
  produced only by that type's constructor, a name already of that type, or
  a literal the checker proved; the census `tests/typed.rs` walks each store
  into a validated place and judges its producer. `vyrn_frontend::validate`
  says which types carry a rule; a sized integer is judged by width and
  signedness.

The fact judgment (`vyrn_lower::fact_flow`) follows the three. It judges
which arrays are sorted, and by which order, and writes only refusals into
the kernel's list. `core::augment` builds one `fact_flow::Facts` from the
program's `std/order` names and its functions' fact clauses
(`core::check::requirements`, `core::check::guarantees`) and the
`effects::Ran` only the effect judgment returns, so it runs after that
judgment. It calls `Facts::judge` on each body it built, before the judgment
memo takes the body's verdict: the memo's declaration fingerprint holds every
clause a fact verdict reads, so a served body keeps its fact refusals. A program
that links no `std/order`, and a body that calls no function with a fact
clause and no `before`, cost no allocation. The walk reads a body's rows:
an empty array literal, a move or store, `@push` (its obligation), `@pop`
and its `Option` switch, `@copy`, a `before` call and the `if` on its
answer, and calls with their requirements and guarantees or summaries. Any other write
drops the facts of the array it writes. A `let`'s promise
(`core::check::Promise`, in `Body::promises`) is judged at each row that
writes its binding, and its comparisons are clause rows after each such
write (`vyrn_lower::check`). Before the loop, `Facts::of` solves the
summary of each private function that returns an array and states no
result fact: its candidates are both facts of every order its walk and its
callees' walks meet, a least fixpoint by `fixpoint::solve`; each body starts
holding them all, and `fixpoint::descend` walks it under its callees'
summaries and drops each candidate a return does not show. A call reads the
summary of the instance it names, by function and type arguments. The memo
keeps each body's summary (`movecheck::Judgment::summary`), so a body built
again reads the summary of a callee the memo served. A loop's head is the greatest state
that holds at entry and after every turn, within `fact_flow::ROUNDS` rounds.
No later pass reads a fact, and the check prover takes none as a premise.

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

The declared functions are emitted on every thread, and the module is the same
on any thread count. A body never touches the module. What it asks of the
module goes into its `Log`: a function index for an instance, a dispatcher or
a shape, a data address, a counted site, a stored-closure tag. Its code holds
a placeholder for each (an index from `LOCAL` up, or a `Frame::hole`).
`merge` replays the logs in source order against the module and the
registries and fills the placeholders, so the module is the one a walk in
source order writes. A body that asked the registries a question an entry
added since would answer otherwise (`Query`) is emitted again in place. The
drain, the initializer and the derived bodies take the same path, one at a
time.

The emitter writes the shape each engine optimizes well: a field read is one
scalar load at a computed offset, never a copy of the aggregate
(`tests/fieldstore.rs` counts the instructions). An optimization of its own
lands where a benchmark licenses it (`docs/decisions.md`). `--wasm-opt` runs
Binaryen's `wasm-opt -O3` over the written module, after the emitter and
before wasm2c; `toolchain::WASM_OPT_FEATURES` names the features it
validates against.

The module's shape:

- Memory (`wasm.rs`): a shadow stack of `STACK_BYTES` growing down from
  `STACK_TOP`, then data from `DATA_BASE` up to `STATICS_LIMIT`, then the
  heap from the address in the global `HEAP_BASE`, 16-aligned past the
  statics. A frame push past address 0 wraps and traps on first
  access instead of overwriting data.
- Values: a scalar lives in a wasm local; an aggregate lives in a frame slot
  and travels as its `i32` address; an aggregate result goes through a hidden
  leading address (`Ret::Out`), except a flat one. A `read` parameter is used at the caller's address unless
  the callee may store into module state that is an aggregate or owns heap
  other than a `String`'s (`Cx::aliasing`), and a `modify` one unless the
  callee may also read any module state, as the effect judgment answers per
  frame (`World::state_use`). Otherwise the callee copies it in at entry,
  and a `modify` one back out at the one exit. A `consume` parameter is
  used at the caller's address, except in an `export` or `gen` function with an
  aggregate result. Every call's destination is proved apart from its
  `consume` arguments (`Fn_::dest_apart`), or the call writes a fresh slot
  and the result is copied after. If every `return` yields the parameter
  (`Sig::in_place`), it
  is the result, the call has no out-pointer, and the caller moves the
  argument into the destination. It passes `x`'s storage instead for
  `x = f(x, ..)`, and for an argument whose extent ends at the call and that
  holds a frame slot, which the result then takes.
- Flat aggregates (`Cx::flat`): a record of one or two scalar fields, or a
  sum of one or two words (`Option<Int64>`, `Option<Bool>`: no payload boxed
  by `types::payload_boxed`), that owns no heap and has no declared release and no `where` rule.
  A flat result crosses as wasm results, one per part (`Ret::Words`), except
  from an `export extern` or `gen` function, whose host or driver passes an
  out-pointer. A flat name lives in wasm locals, one per part
  (`Place::Words`), when every row that names it is one `Fn_::core_flats`
  admits. It is written by the variant or record row that builds it, or by a
  call whose result is `Ret::Words`. It is read by a switch on its tag (the
  first word; a payload binder decodes its word into a local of its own), a
  scalar field read, a store into a place with an address (part by part) and a
  `return`. It is released by nothing, because it owns no heap. Any other row
  keeps the name in its frame slot, and a call lands its parts there. No row
  takes the address of a name in locals, so nothing spills: a row that needs
  an address (an argument, a part of a larger value, a join, a copy) decides
  the slot up front. The slot home goes when each such row has a form over
  the parts; arguments by value come first.
- Layout (`layout.rs`): `shape_of` maps a type to a `Shape` of `Leaf`s, and
  sizes, alignments, offsets, loads, stores and the call ABI are all read off
  it. Equal shapes are one representation. Every size is a checked `u32`.
- Control flow: `St::If`, `St::Loop`, `St::Block` and `St::Switch` map onto
  wasm's `if`, `block` and `loop`, and `break` and `continue` onto `br`. A body never emits `return`, which would skip the frame's
  epilogue; a `return` is a `br` to the body's outer block.
- Waiting bodies: a function that waits takes one more `i32`, its frame, and
  returns one more `i32`, its status: 0 done, 1 cancelled, else the frame's
  address while it waits. The frame is a heap block (`Frame::heap_prologue`).
  Each awaited call row is a resume point, numbered in source order. A
  suspension saves every local into the frame (`Frame::save_locals`) and
  returns. A resume restores them, and at each structured construct on the
  path to the point a `br_table` enters the arm that holds it
  (`Fn_::resume_dispatch`), so no relooper exists. A cancelled callee sends
  the caller down the row's cancellation edge, then out with status 1
  (`Fn_::await_close`).
- The run queue: `std/task`'s module state holds a task per call that started
  one and a wait per host operation a task waits on, found by the operation's
  id, and `wake` runs the woken tasks in wake order. A task's root is `main` (0) or a waiting export (from 1),
  whose export is a step that begins the task. The step takes the host's id
  for the task first; the root's end hands its result word to `taskResult`,
  and `taskEnd` calls `hostDone`, the import `vyrn_wait.done`, with the host's
  id, the status and the result. `__vyrn_wake` is `std/task`'s
  `wake`, whose `hostResume` call is the emitter's `$resumeRoot` dispatcher
  over the roots. `hostSleep` is the import `vyrn_wait.sleep`, `hostFetch` the
  import `vyrn_wait.fetch`, whose wake's payload `payloadOf` reads, and `hostCancel`
  the import `vyrn_wait.cancel`, which drops a cancelled wait's operation.
  After a trap the host calls `__vyrn_trapped`, `std/task`'s `taskTrapped`,
  which forgets the running task and every wait it held and returns the host's
  id for it, so the trap ends that task alone.
- Regions that wait: a `region` that holds a point keeps the arena outside it
  in a local (`Region::Own`). `std/task`'s `regionSwap` exchanges the two
  where the region opens, at each suspension inside it (`Fn_::regions_suspend`)
  and at each resume that enters it (`Fn_::region_enter`); `regionClose` and
  `regionCarry` end it. They use `std/runtime`'s `arenaSwap`, exported only
  to `std/task`, and `regionExit` and `free`, so the emitter writes no arena
  layout.
- Function values: a waiting type's dispatcher (`World::waits_through`) takes
  and returns a frame as a waiting body does, and holds a block of its own
  with the tag and its target's frame while the target waits
  (`lower_dispatcher`).
- Forks: an awaited `both` or `race` is one call row of kind `Callee::Fork`,
  whose arguments are every branch's in order and whose `core::Fork` holds each
  branch's callee and argument count, so the kernel judges the branches as one
  call. The row is one resume point (`Fn_::core_fork`). Its start calls each
  branch with frame 0 in argument order; a resume polls each branch still
  waiting. A branch's call is `Fn_::core_user_call`'s, and its result lands in
  the branch's `result` name until the fork builds the row's value from them.
  To cancel a branch the fork polls it between `std/task`'s `cancelBegin` and
  `cancelEnd`, where every host wait answers cancelled, so each frame on the
  branch takes its cancellation edge.
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
| Native | `vyrn build`, `vyrn bench` | `build_wasm2c`: wasm2c turns the module into C, and clang compiles it against a precompiled header and links it with `wasi_host.c` and wabt's `wasm-rt`, whose objects `nativec` caches |
| Browser | `web/`, the site's playground | `web/wasi-min.js` answers the same imports with a browser's limits: no argv, EOF on stdin, no files |

`vyrn_codegen::toolchain` finds wasm2c, clang, simde, wasmtime and
`wasm-opt`. `vyrn.lock` pins wabt, simde, wasmtime and binaryen by sha256
under `tool:` rows, unpacked under
`~/.vyrn/tools/`. clang is recorded, not pinned. `--native-target` picks the
x86-64 level (the default is `v2`). clang always runs with
`-ffp-contract=off`, so a native float result equals the wasm result.
On Windows x64 the driver declares each multi-value function `sysv_abi`
before clang runs (`toolchain::sysv_multi_returns`), so a two-word result
returns in registers, as it does on Linux.

The native route compiles only the program's C. The precompiled header
(wasm-rt, libc, simde) and the objects of the host and of wabt's two runtime
files live in `~/.vyrn/cache/native/<key>/` (`nativec.rs`). The key hashes
`clang --version`, the wasm2c version, simde's `simde-common.h`, every compile
flag (so `--native-target` and `VYRN_DEBUG_SYMBOLS` select a directory) and
wabt's `wasm-rt*` files; a changed input names a new directory, so no entry
is stale. The host object's name hashes the host text and the program's
`.w2c.h` less its include guard, because the host reads `w2c_prog`'s layout
from that header. Every entry is renamed into place, and a `.sum` file beside
it holds the object's SHA-256 (the header's length), so a damaged entry is
rebuilt. The first build of a toolchain fills the directory at the cost of a
build without the cache; a directory of a retired toolchain is deleted by hand.

A module that imports `vyrn_wait.fetch` also links the client `wasi_net.c`,
rustls-ffi's static library and an object of the roots. `vyrn-cli/build.rs`
builds the library with fat LTO through a cargo of its own, takes `rustls.h`
from rustls-ffi's include directory and writes the roots of
`webpki-root-certs` as PEM; `vyrn` embeds the three, so a user's machine needs
no cargo. The library and header names carry `VYRN_TLS_KEY`, a hash of
the three files. The client's object name hashes that key and its text; the
roots' object name hashes the PEM, so a `VYRN_TEST_TLS_ROOT` build gets an
entry of its own. `wasi_net.c` loads the PEM through rustls-ffi's
`rustls_root_cert_store_builder_add_pem`.

### Host waits

A host wait is a builtin of `Spec::HostWait`: `hostSleep` (`std/task`) and
`hostFetch` (`std/fetch`), each an import of `vyrn_wait`. Its call starts the
operation, takes the host's id and waits on it; the wake's payload is its
result. `hostFetch` is the floor's `net` carrier.

A value that crosses the boundary as a String is a list of fields: each its
byte length in decimal ASCII, `:`, then its bytes (`vyrn_cli::wire`). `vyrn
serve`'s packed response is one. `std/fetch` packs a request as the method,
the URL, the header count, each header's name and value, and the body. A reply
is `ok`, the status, the header count, each header's name and value and the
body, or one of `badUrl`, `unreachable`, `untrusted` and `badResponse` alone.
Its headers are the Fetch standard's sort and combine, which a page's `fetch`
applies itself: names lower-case and sorted by bytes, the values of one name
joined by `, `, and `set-cookie` left out. A body or a value that is not UTF-8
makes the reply `badResponse`. The embedded host (`vyrn-cli/src/fetch.rs`),
the native host (`wasi_net.c`) and `web/wasi-min.js` each produce these bytes,
and `tests/fetch.rs` holds the three to one output.

`vyrn test` and `vyrn bench --check` compile the selected bodies into one
module, each an exported `__vyrn_body_<k>`, and run them in one instance, so
a body sees the module state the one before it wrote. A trap ends its body,
prints the `FAILED:` line, and the next body runs.

A `gen fn` runs inside the load. `vyrn-genwasm` compiles the generator's
module with a synthesized `main` that dispatches on its first argument, runs
it in wasmtime, and reads the result: a `String` between two sentinel lines on
stdout, a `Code` value through the `vyrn_gen.result` import. File reads
go through the loader's resolver and are recorded as the generator's inputs;
`lex`, `moduleInterface` and `contractOf` cross as a host encoding and a
synthesized decoder. A generator the engine declines is an error: "the
installed generation engine declined it".

A `Code` value is a rope of spans (`gen::Rope`): concatenation is a node, not
a copy. Each span carries its author (`gen::Author`) and where its errors show
(`gen::Shown`). The loader stamps each quote part `@codeText(text, line, col)`
with its module's key after the parse, in the walk that stamps `panic`, so a
quote's text is written by the module that holds it. A `String`, number or
`Bool` in a hole, and `raw` text, is the call site's. A `Code` result reaches
the loader as `gen::Written`: the text and a span table sorted by offset. The
cache entry stores the table inside the authenticated body, and `OriginMaps`
reads its regions from the table's `shown` column (`add_spans`). A `String`
result has no table: its regions come from `//@origin` lines and its imports
are judged by its banner. A report is a `Diag` span (`gen::Kind::Diag`): the
loader lifts a `Code` result's diagnostics from its `Diag` spans alone, and
`render` prints a `Diag` span as the `//@diag` line a `String` generator
writes. Both paths stand until the last `String` generator in std moves to
`Code`; that commit deletes the directive scan, the `//@diag` text and the
banner rule.

The link judges each import of a generated module by the span under its
`import` keyword (`loader::author_of`, the parser records the keyword's column
in `ImportDecl::col`): a `File` span as that module, a `CallSite` span as the
file that wrote the generator call. `admits` stays the one visibility rule. A
refusal there carries a note naming the author and its line (`written_by`).

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
- `fnverdicts.rs`: each function's trap-free and kernel verdicts, against
  `tests/pins/fn-verdicts.tsv`.
- `canon.rs`: how many relations and conditions of the corpus share an
  identity at each layer of `canon`, against `tests/pins/canon.tsv`.

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
| `VYRN_THREADS=<n>` | types, walks, builds, places and emits bodies on `n` threads; `1` keeps a trace in body order |
| `VYRN_SHUFFLE=<seed>` | permutes the order the checker's, the lowering's, the placer's and the emitter's threads take bodies in |
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

`verdict::table` (`verdict.rs`) gives each function of the root file its
trap-free and kernel verdicts for the same report. It walks the decided bodies
the root's functions reach by direct calls and declared releases: a frame's
own cause is its first explicit trap call, kept check row, call through a
function value, extern call or call into a row with no body. The distance in
calls to the nearest own cause is solved by `fixpoint::solve`, so a cycle with
no cause is trap-free (the greatest fixpoint). It cannot join the effect
judgment's walk, which runs before `check::state` adds the rows. The kernel
rules read the effect set the judgment kept (`World::state_use`), the call
graph's components (`fixpoint::components`), callees first, and the loop form
`elide::counter_loop`, beside the `elide::bound_test` it starts from. A
kernel's `verdict::Form` joins its own types' forms with its callees'.
`tests/fnverdicts.rs` pins both verdicts of every function of the corpus.

`verdict::erased` judges every row a call for its effect (`St::Do`) enters, in
the bodies `World` serves, by the kernel's walk with the kernel's types
replaced by `read` parameters (`verdict::Rules`). The emitter takes the table
once, after the checks phase, and emits nothing for a row
`verdict::Erased::erases` names, nor reads it for a first read. `vyrn why
--cost` prints a call into a function with a result clause that stays
(`insight::Kind::Stays`), with the rule `Erased::stays` names.

`vyrn why --provenance` prints `provenance::table`: per function of the root
file that returns an array, the class of the backward map from the result's
positions to each array parameter's, read from the same bodies as `--cost`.
`provenance::of` interprets one body abstractly: per array it keeps each
parameter's monotonicity, last and first index pushed, and for an in-place
store the one duplicated position and the names that hold the content it
displaced. A builtin's effect on positions is its row's `ShapeMap`. A loop
whose head test is its only exit, whose counter steps by one, and which
pushes one element of a parameter at the counter every turn covers the
parameter from the counter's start to the bound. `tests/provenance.rs` pins
the classes of the 48 functions the analytic report classified by hand.

## Relation identity

`canon.rs` decides when two relations a fact names are one: a predicate, a
key lambda, a loop-free function, or a condition. No compiler pass
reads it; `tests/canon.rs` is its census. It reads the decided bodies.
`Table::relation` replaces each name of a body by the tree of its value,
numbers the parameters by position and names a capture by its spelling; an
`if` with a branch that returns runs the rest of the body under each branch.
A loop, a `match`, a store outside a local or a call for its effect leaves
the body without a tree. `Table::conditions` keeps the names the reader bound
as leaves. Every node lives hash-consed in one `Table` per program, so an
identity is an `Id`, and `Table::key` spells it across programs.

Each layer is a rewrite that keeps the value and the traps on every input:

- Layer 0, `Level::Syntax`: the tree, with the operands of `==` and `!=` in
  key order. The core already writes `a && b` as `if a { b } else { false }`.
- Layer 1, `Level::Boolean`: `!`, an `if` at a Bool position and `==` between
  Bools are structure; every other Bool is an atom, `a != b` as `!(a == b)`.
  The structure becomes two reduced ordered decision diagrams, answers true
  and answers false, over atoms ordered by key. An atom with an element that
  may trap (an index, a key, a division, a remainder, a shift by a
  non-literal, a call `verdict::TrapFree` does not clear) answers only where
  a definedness variable per element holds.
- Layer 2, `Level::Atoms`: an integer comparison compares linear forms
  modulo `2^w` over opaque terms; an equality moves every term to one side,
  an order comparison flips to `<` and steps a constant. A `String` comparison
  flips and negates, a float one only flips. An integer key body is its
  linear form.

Past `BUDGET` (16 atoms, 1,024 nodes) a Boolean structure degrades in steps:
trap-free conjuncts over disjoint atoms are decided apart, then sifting
looks for a variable order within budget, then each part that fits is
decided and the rest stays syntax (`Step`).

## Where a change goes

- A new surface form: a desugar in the parser, or a lowering in `core::build`.
  The kernel and the emitter learn nothing new if the form lowers into
  existing statements.
- A new ownership rule: `kernel.rs`, with a program in `tests/refusals/` and a
  row in `refusals.rs`. The checker states no ownership rule.
- A new refusal or acceptance test: a program in `tests/check/` and its
  `.stderr`, never a program in a Rust string.
- A new builtin: one `prelude::Builtin` row, with its contract (a capability
  per parameter), method spelling, `Spec`, effect, route, shape map and
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
