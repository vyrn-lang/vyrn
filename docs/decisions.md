# Design decisions

Each line is a decision that binds the code today, with its reason. A line that
starts with "No" records a road not taken; reopen it only with the evidence its
reason asks for. A change that contradicts a line changes the line in the same
pull request.

## Language

### Scope

- A feature earns its place by helping most programmers every week. Macros, higher-kinded types, full dependent types and custom allocators stay out, so the core stays small and predictable.
- No class inheritance. Behaviour is shared through protocols and composition, because base classes are fragile and diamonds are ambiguous.
- No attributes. Where a rule over a declaration is not enough, name a type.
- No new keyword for a library domain (RPC, config files, i18n). A generator import is the mechanism; an `rpc` keyword was built and removed.
- No `async`/`await`, coroutines or blocking run loop. The host owns the event loop and calls exported handlers; wasm cannot suspend, and determinism is the product. Reopen when wasm stack switching ships broadly or handlers need an outbound HTTP client.
- No unwinding. A trap ends the process and runs no release path.

### Types

- Records are structural with width subtyping: a record with more fields is usable where fewer are expected. Types describe shape, not identity.
- No casts and no `as` on values. A conversion is structural widening, a compile-time proof, or a checked narrowing that returns `Option` or `Result`, so memory is never reinterpreted.
- The type transformers `Omit`, `Pick`, `Merge`, `Partial` and `A & B` are erased before lowering. `Readonly<T>` is `T`, because records are immutable.
- Every numeric type names its size (`Int8` to `Int64`, `UInt8` to `UInt64`, `Float32`, `Float64`). There is no unsized `Int` or `Float` and no implicit widening.
- Validation lives in the type (`type Age = Int64 where value >= 18`). A constant is proven at compile time at no cost, anything else is checked at every value boundary, and a raw value never becomes the refined type without construction.
- An alias without `where` is transparent: `type Id = Int64` names `Int64` and has no constructor. A nominal alias type-checked `Id(x)` that no backend could lower, and a JSON decoder had to answer in a type its body did not produce.
- Refinements stop at predicates and integer-sized arrays. Full dependent types would turn the compiler into a proof assistant.
- `SmallArray<T, N>` is the only type with an integer argument (1 to 64). There are no user const generics, and `Array` has no hidden small-buffer optimization, because that would ripple through the ABI.
- Absence is `Option<T>` and failure is `Result<T, E>` or `Validation<T>`. There is no null and there are no exceptions.
- `Option` and `Result` are ordinary declared sums (`| None | Some(T)`, `| Err(E) | Ok(T)`), so one rule serves every sum in every pass.
- Accumulating validation is the prelude's `Issue { key, path, message }` with `Validation<T> = Valid(T) | Invalid(Array<Issue>)`. A form needs every error at once, and `key` is a stable i18n id.
- An interpolation whose holes are finite string types is checked by DFA containment. A proof costs nothing at run time, and a failure names a witness string.

### Strings and bytes

- A `String` is immutable UTF-8 bytes. There is no `.length`: `byteLength` is O(1) bytes and `charCount()` is O(n) scalar values, because one name answered two questions.
- `s[i]` is a `UInt8`. String comparison is bytewise and never uses locale collation.
- `'c'` is one ASCII byte of type `UInt8`. There is no `Char` type, and `'ab'` is an error.
- A NUL byte is refused in a `String`, and UTF-8 validity is checked when bytes become a `String`.
- Interpolation is `\{expr}` inside an ordinary string, so `{` and `}` stay literal for LaTeX, SQL and JSON.
- A tagged template `tag"..."` calls `tag(parts, values)`. The parts are compile-time literals and `Value` is the closed enum `IntVal | StrVal | BoolVal`, so a value can never become structure.
- A user type in a template hole renders through its `impl Show` and is boxed as `StrVal`. `Value` stays closed, because without existentials there is nowhere to put a wider element.

### Functions and protocols

- Protocol conformance is explicit (`impl P for T`) and checked where it is written: a missing method or an extra method is refused.
- No inherent methods. `x.m(a)` is `m(x, a)`, so a helper needs no `impl`.
- Dispatch is static everywhere; there are no vtables. Records are legal impl targets and validated scalars are not.
- Generic impls are keyed on the type constructor: one impl per protocol and constructor.
- Protocols take associated types, not type parameters. No `protocol P<T>` and no `T::Output`: an operator like `?` has nowhere to name the instance.
- A type declares its properties by implementing compiler-known protocols (`Owned`, `MustUse`, `Fallible`, `Copy`, `Iterate`, `Hashable`, `Show`). Built-ins have seeded rows and a declared row wins, so no hand-written list can fall out of date.
- A scalar renders through the language's own lowering; `impl Show` is consulted only for a type the language cannot render. Records get no derived `Show`.
- Any expression may be a method receiver, evaluated exactly once.
- A call names a function or a binding. No call on an arbitrary expression (`r.f()` on a field, `xs[0](x)`); bind it first.
- A function value lowers by defunctionalization: one closed tag per source and a direct call per signature. No function pointer or indirect call exists in any module, because whole-program compilation knows every callee.
- Captures are read-only snapshots taken where the lambda is evaluated. A lambda may not contain another lambda literal, and a function type may not take or return a function.
- Function values have no `==` and never cross the wire.
- A lambda is `x -> e`, `(a, b) -> e` or `() -> e`.

### Control flow and operators

- `if` as an expression needs an `else`, and each branch is one braced expression. No ternary and no block values.
- A block has no value. A match arm may be a block only in statement position; an expression `match` keeps single-expression arms.
- `if let` is a statement; `while let` is `while true { if let ... else break }`.
- A refutable `let Variant(x) = name` traps on a miss. Its scrutinee is a name, and it matches user enum variants only.
- `break` and `continue` are unlabeled. No `loop {}`, no `break value`, no `let ... else`.
- No compound assignment (`+=`). If it comes, it comes for every operator at once.
- Bitwise operators take two operands of one sized integer type. Signed `>>` is arithmetic and unsigned `>>` is logical.
- Bitwise operators bind tighter than comparison and looser than `+`, so `x & mask == 0` means `(x & mask) == 0`.
- A shift by the bit width or more, or by a negative amount, traps, and is a compile error when the amount is constant. C's undefined behaviour and x86's masking are both rejected.
- `%` is truncated remainder on integers only. `a % 0` traps; `INT_MIN % -1` is 0.
- `??` is handle-or-default on `Option` and `Result`, desugared to `match`. It binds tighter than comparison and is right-associative.
- `?` propagates the whole failure unchanged; it never converts error types. On a user type it goes through `Fallible`'s `isSuccess` and `success`, never through variant order.
- `a[i]` traps out of bounds rather than returning a `Result`. An operator in a hot path does not return its failure.
- `panic(msg)` has type `Never`, prints `error: msg (file:line)` for the line where it is written, and exits 1. Only user code panics; std returns `Result`.
- No `==` on a `Map`.
- `Map` iteration, printing and JSON follow insertion order, so hash order is never observable.
- A `Map` key is `Hashable` and owns no heap: sized integers, `Bool`, `String`, records of those, and fieldless enums. Floats (NaN breaks reflexivity), heap fields and payload enums are refused by name.

### Modules, state and effects

- The loader flattens modules into one program; the checker and emitter never see a module. Import cycles are refused, and impl coherence is global.
- `import * as ns` binds a compile-time name that is not a value, one level deep. No `import * from` wildcard and no re-exports.
- A private name that collides across modules is renamed by the linker. A collision with a name the module itself imports is an error.
- Importing `Result` or `Option` from `std/result` or `std/option` is legal and changes nothing; ambient use stays legal.
- A name whose body lives in std is an import, not a global, and a missing import names its module. `print`, `bytes`, `panic` and `parse` stay global.
- Module state is a top-level `let` in any module, one instance per process and private to its module. `export let` is refused: export accessor functions.
- Module state initializes before `main` in import order, lives for the whole run, and cannot be consumed or dropped.
- Once-only work at run time is module state. No `once` keyword, because grammar is the most expensive thing to add.
- `extern fn` is the only way into the host, under the wasm import namespace `vyrn`. On a target with no host, a call traps with `extern \`name\` is not available on this target`; it is never stubbed.
- Every wasm export and `vyrn.*` import declares its signature in the `vyrn:exports` custom section. A host reads types from it and never infers them from the ABI.
- An artifact is an entry plus a target (`native`, `wasi`, `browser`). The target is a capability set; `wasi` and `browser` are the same bytes.
- The capability floor: the capabilities an artifact's linked closure uses must be a subset of its target's set. The sets are compiler constants no manifest can edit.
- The floor tracks four capabilities: `fs`, `stdin`, `args` and `extern`. Output, clock, entropy and threads are universal, because a row that refuses nothing says nothing.
- A capability is carried by what the source contains, not by which branch runs. For `extern` it is a call to an import, not the declaration.
- The time and random host imports are provided on every target and are not the `extern` capability.
- A module's audience (server, client, universal) comes from the nearest directory segment `vyrn.json` names. An import that widens the audience is a checker error.
- Audience is a fence against accidental imports, not a secrecy guarantee. The compiler cannot know what a secret is; the capability floor sits under the fence.
- A module contract (`contract`, a contextual word) is an ordinary library declaration, checked by `std/contract` over `moduleInterface`. The compiler hardcodes no convention.
- Contracts are closed by default: an export the contract does not name is an error with a did-you-mean. An open rule serves only where names carry no meaning.
- Mutation on a procedure is declared with `mut fn` and is transport-free. An unmarked procedure is a query; nothing is guessed from a name.

### Test, bench, logging

- `assert` and `assertEq` are legal only in a `test` body; tests and benches are checked and then stripped from `run` and `build`.
- `blackBox` is legal only in a `bench` or `test` body.
- Logging is `logger(name)` with five levels, and the level and sink are set in one static `logging { level, sink }` block. Logs go to stderr.

## Memory

- There is no tracing garbage collector. Memory is released at points the rules define.
- Ownership is defined by rules, not inferred. A program compiles with known reclamation or fails with a fix named in the error; a best-effort inference leaked whenever it was unsure.
- A type that owns heap, directly or transitively, moves on assignment, argument passing and return. Scalars and records of scalars copy.
- A parameter is `read` (the default), `modify` or `consume`. The capability is the API contract, and bodies need no annotation.
- `read` and `modify` borrows are second-class. They cannot be stored, captured by an escaping closure, returned or handed to `consume`, so no lifetime annotation exists.
- A function returns an owned value. No borrowed return; projections cover in-place access.
- A place owns its contents. A store releases what the place held, and releasing an aggregate releases its places.
- Copying is explicit, spelled `x.copy()`. A hidden copy is an unbounded cost that nothing at the call site shows.
- No reference counting, deep copy on assignment, region inference or linear-everything. Each adds run-time cost, hidden behaviour or annotations on the common path.
- No drop flags. An ambiguous join (moved on one path only) gets a release on the other edge or is refused.
- A join arm that yields a name bound outside the construct moves it; a later use is refused with the `.copy()` fix. Treating the yield as an alias leaked the name on the edges that did not yield it.
- A projection (a field, element or pattern binder of a place) borrows its root. It may be read, not stored or returned; the fix is `.copy()` or a take.
- A refusal quotes what the reader wrote, never a compiler temporary (`@t1`): `rules::spoken` panics in a debug build on a sentence that does.
- A lend is refused, never tracked through stores. Tracking it would need an alias analysis.
- `consume <place>` moves a value out of a place; only the taken path dies. No `take` keyword and no `.take()`: an element leaves with `swapRemove`.
- A statement's value is released where the statement ends. A part read as a statement (`p.f`, `xs[i].f`) takes nothing: only `consume` moves a value out of a place.
- A heap part of an element of a call's result (`mk()[0]`, `mk()[0].s`) is copied, and the result is released whole: a release cannot skip a hole in one element. An element of a type with `impl Copy` is copied by the impl where it is taken; a field under one and a scrutinee stay borrows.
- Iterating a place binds a `read` borrow. `for x in consume xs` takes the container; a loop over a temporary owns its elements.
- A `let` of a place read from a `read` parameter is a borrow. Writing through the root while it lives is refused, and the fix names `.copy()`.
- When unsure, the compiler leaks rather than double-frees. A leak is a counted row; a double free is a crash.
- The system is affine: an undisposed value is released at scope exit. `impl MustUse` is an opt-in obligation, separate from `consume`, and it passes through containers.
- `Stream<T>` is linear: consume it with `for`, return it, or `close()` it. It cannot be stored in a field, element, type argument or module state.
- A must-use refusal quotes the binding's resolved type (`Stream<Int64>`), not the producer's spelling. A generic body is judged per instance and refused once per binding.
- A generic parameter does not launder an obligation: a linear value passed through `T` is owed like any other linear binding.
- A self-referring type declares `impl Owned`. The declaration gives the structural release walk its bottom.
- Aliasing is a `Handle<T>` into a container you own (`std/slots`): slot, generation and owner in three plain words. `s[h]` traps on a dead handle and `get` returns `Option`. The library measured 2x faster than the compiler slab it replaced.
- The run-time memory surface is `malloc`, `realloc`, `free` and `memcpy`, plus the explicit `region` arena. No engine checks a generation counter.
- `region { ... }` is explicit, a real bump arena, routed lexically. Inference is refused: 0 of 3,758 corpus bindings qualify, and the arena costs up to 1,023x the memory. Reopen with 20 qualifying bindings and a benchmark the arena wins.
- A heap value inside a `region` cannot go to a `consume` parameter, and in-place append is refused inside a region.
- There is no view type. A stored view is a source plus a locator (a byte range, a path, a handle), checked at use. A locator adds no heap edge, so it carries no proof obligation.
- A projection is spelled `-> read T` or `-> modify T` on an impl member and is inlined at the access site. Projections are never inferred: an inference could turn a 1 ns alias into a 490 us copy with no diff at the call site.
- An optional projection `-> read Option<T>` is read only by `if let` or `while let`. A protocol may declare a projection, but dispatch through a `<T: P>` bound is refused.
- No call-shaped assignment (`x.f(i) = v`). Write `setF(modify self, ...)`.
- A `String` is one word pointing at NUL-terminated UTF-8, with length and capacity behind the pointer; capacity 0 marks a static literal. Three words would box every enum payload.
- `m.keys()` copies its keys; `Array<T>` owns its elements.
- A map store takes its key.
- At most 1,000 calls may be in flight, counted at the callee on every engine; a frame may claim 8,192 bytes. One limit belongs to the language, not to whichever stack runs out first.
- An array literal holds at most 512 elements, refused by the checker so `check` predicts `build`.
- A `region` nests at most 64 deep.
- Allocation failure prints `error: out of memory` and exits 1 on every engine. The bound may differ by engine.
- Releasing a deep tree recurses, bounded like any recursive walk. No worklist release.
- No raw-memory view or `unsafe`. `Array` is the primitive and containers over it are ordinary Vyrn; `SmallArray` stays built in because it needs an uninitialized place.
- No transmute, unchecked indexing, inline assembly or atomics.

## Compiler

- One pipeline: the loader, the checker, the lowering to a named core, a kernel that judges the core, and one wasm emitter. Each rule lives in one place instead of once per engine.
- A host enters the pipeline through `vyrn_lower::{load, load_warned, check_and_synthesize}`. The frontend's `check_and_synthesize` types and synthesizes and judges nothing; a host that calls it alone skips every ownership and floor refusal.
- The editor runs the pipeline `vyrn check` runs, passed in as `vyrn_lower::JUDGE`. What only the editor needs (recovery, the index, the judgment memo) wraps the pipeline and never replaces a step of it.
- The core binds every intermediate value and makes every memory access a place. A field read is one load, never a record copy; the copy made wasm 13x slower than native.
- The kernel re-checks every body on every compile with three judgments: ownership, effects and types.
- The ownership judgment is one forward solver over the structured core, a join at each merge and a widen to a fixpoint at each loop, and every refusal is a rule row judged at a use, a scope end, a join or a back edge. No rule gets its own walk.
- A flow-free ownership or typed rule (shape E) filters every row, including rows after an ended path: a dead row is still a program the language refuses. The flow solver judges only reachable rows. Both read their sentences from one table, `vyrn_lower::rules`.
- A whole-program analysis over the call graph runs on `fixpoint::solve` in `vyrn-lower`: components bottom-up, a join the analysis supplies, widening by round number. No analysis writes its own fixpoint loop.
- Releases are placed once, from the core, by one liveness pass. No emitter places a release.
- What one analysis decided about a program (the kernel's placement, the checker's record) lives on that program's `own::Ownership`, and a pass reads the one it is handed. A thread-local keyed by a program's address answered for another program.
- Program state lives in one value, the World, owned above the front end by `vyrn-lower`. The front end declares no slot for lowering to fill, and no thread-local or global holds a program's state.
- A table is keyed by a resolved id (function, type, declaration, name), never by a spelling; two bindings that share a name merged their facts. A node id is a function id and a local index, so an edit to one function renumbers nothing else.
- An id is a storage index, never an order. Diagnostics and emitted functions follow source order, so an incremental check and a fresh one print the same bytes.
- Each relation has one writer, which sets both directions and deletes in a batch. No hooks and no second storage shape for one relation.
- The call relation is between source functions, as an edit is: every instance of a generic and every lambda frame call under their function's row. A call through a value is no edge; the effect judgment keeps its own per-instance graph with the values' closed sets.
- A recheck pulls: each result records what it read (a signature, a summary, a name lookup in a scope, misses included), and a cache that records no reads is off in incremental mode.
- Function bodies check in parallel in callee-first waves. Workers create no ids; a serial merge does. Output is byte-identical on one thread, many threads and a shuffled order.
- A worker reads the loading thread's thread-local inputs only through `project::Lent`. A projection site its lent memo lacks expands nothing, and the body is built again on the loading thread, in body order.
- The World has no query engine, runtime scheduler, archetype storage or on-disk snapshot. Each pass is a function over the tables it borrows.
- A rule stays in the checker when no other pass refuses the program on the same line. A moved rule keeps its surviving home's sentence.
- The generation fence stays in the checker, because it is the only judgment that runs before a generator executes.
- A surface form that another form can state is a parser desugar, so each walker states one form. `if let` and `while let` are a statement `match` with a `Pattern::Other` arm, and take their scrutinee at its last use as `match` does.
- The interpreter is deleted and there is no `--engine` flag. It was a third value model, and a flag with one legal value is a rule stated twice.
- The oracle is recorded output (`examples/expected/`) and the per-example wasm SHA-256 manifest checked on every CI platform.
- Wasm is emitted directly with `wasm-encoder`. No LLVM, clang or sysroot is needed to build or test the compiler.
- A replaced path is deleted, not kept behind a flag. Gated multiplicity stays true and ungated multiplicity rots.
- The emitter never optimizes; the engine that runs the wasm does. No shared emitter trait or instruction-builder abstraction.
- The sweep never moves a datum. A data address is an untyped constant, so relocating by value could rewrite a user's integer. A dead datum costs no module byte, and the static area ends at the last live datum or reservation.
- Native code is the same wasm through `wasm2c` and clang `-O2`. No Cranelift route for `build`: against an LLVM baseline it measured 2 to 3x, and `wasm2c` 1.5 to 1.9x.
- `run`, `test`, `bench --check`, `serve` and `dev` run the module in embedded wasmtime.
- A trapped call into a resident instance (`serve`, `dev`, `test`) keeps the instance. The host restores the stack pointer, call depth and region nesting it read before the call; module state and heap blocks stay as the call left them. Re-instantiating would drop the state every earlier request built.
- Control flow stays structured in every intermediate form, because wasm accepts only structured control flow.
- Monomorphization happens once, above the emitter, and an instance is identified by its type arguments, never a mangled string. A mangle collision once miscompiled silently.
- Monomorphization has two bounds: 64 levels of nesting and 65,536 parts. `vyrn check` runs them, so a passing `check` means `build` terminates.
- Every trap wording lives in one table (`vyrn_frontend::trap`), and a test fails on a re-spelled wording. An engine chooses how to raise a trap, never what it says.
- Every runtime check is its own core row, stated once before the row it guards; one runtime check is one row. The emitter runs no check without a row, and a row no construct runs is an error.
- A change that proves checks is licensed by the check oracle (`VYRN_CHECKS`, `scripts/check-elision.sh`): no proved row fails a run, and the elided, kept and oracle builds print the same.
- A check row is proved only with a certificate that a checker sharing no code with the search accepts. Until builtin rows state length effects (#12), every `modify` or `consume` argument forgets its name.
- The prover reads a record's `where` rule: `a.length == b.length` makes one length term of both fields. The rule holds wherever the record is, because every boundary checks it and the only store into the record in place is into an element of an array field the rule reads through its length.
- Every limit is one constant, derived where it is used, and a test checks the derivations.
- Error text is canonical Vyrn wording, never the operating system's.
- The parser refuses nesting deeper than 1,024 with a diagnostic, because remote modules and the LSP parse untrusted input.
- Diagnostics speak intent: what you asked for, what blocks it, how to fix it. They say read, modify and consume, never "borrow" or "lifetime".
- A refusal of the lexer, parser, loader or checker is a row in `rules.rs`: a `Rule` names its holes, its sentence and its fixes, and the diagnostic carries the rule with its hole text. Sites that print the same sentence name the same rule. Text written outside the frontend (`vyrn-lower` sentences, generator output, manifest and schema errors) stays a string in `Diagnostic::error`.
- The runtime (allocator, strings, maps, arrays, I/O, traps, regions) is Vyrn in `std/runtime`. The raw memory and WASI primitives are declarations in `std/mem`, importable only by `std/runtime`.
- The WASI calls a module imports are one table, `vyrn_codegen::WASI_IMPORTS`. The emitter declares from it, and a test holds each host (`wasmrun.rs`, `wasi_host.c`) to exactly its rows.
- A builtin exists only for what Vyrn source cannot express, such as a syscall. Everything expressible is std Vyrn.
- Every fact about a builtin (contract, method spelling, core `Spec`, effect, route, length and element effect, editor text) is one `prelude::Builtin` row. A pass reads the row, never its own list of names.
- Hot per-element paths stay inline in the emitter (indexing, the call-depth counter, the map value paths), because one wasmtime call level costs 14 to 270% there.
- A `for` over any indexed container is one index walk in the core. A user container supplies its `size` call and its `nth` element read; it gets no loop of its own.
- A `read` or `modify` aggregate parameter is the caller's storage, used in place, while no module state can name that storage; the checker's exclusive-`modify` rule covers the parameters. A per-callee effect gate waits until the effect judgment attributes every call.
- Runtime modules are injected on mention under unlexable `$` names, so user names neither collide with them nor capture them.
- A generator runs as compiled wasm in embedded wasmtime. Each capability that needs compiler machinery (read, list, `moduleInterface`, `lex`, `contractOf`, code quotes) is a host import, so there is no second implementation.
- Generator budgets use wasmtime fuel, never wall clock, so a limit is deterministic.
- A generator is comptime-pure: no `extern`, module state, file writes, stdin, `args` or logging. It reads files only under its constant path arguments, and every file it reads joins its cache key.
- The generator cache is keyed by everything a generation observed and authenticated with a per-user secret. It is never restored across CI runs: a restored cache is untrusted compiler input.
- Type reflection is a flat array of `TypeNode`s whose edges are indices, so no recursive Vyrn type and no new decoder shape is needed. A declared name is a leaf; kinds are strings, like `Schema.base`.
- A derived-code generator receives one `TypeArg`: a graph over the checked types its call sites need, a node per `struct_key`, each node's kind `codec::wire`'s verdict. The codec rules stay in Rust; the generator only writes text.
- A `derive` generator runs once per program over every type its sites need, cached in-process by its program and argument. A run per type would write a shared subtype twice. Its compiled module persists across processes under a hash of the trimmed program's canonical text, which sorts the AST's hash containers. What it writes is checked against the program: only its bodies are typed, because a body's verdict depends on the declarations and not on other bodies.
- Derived code lives in std as a `derive` generator, not in Rust. `toJson(x)` is `derive(jsonEncoders, x)` with `std/json`'s generator, and `fromJson<T>(s)` calls what `std/jsondec`'s `jsonDecoders` writes for `T`; a generator's own program may reach `derive`, but not its own generator.
- A `derive` generator may write type declarations as well as functions; both are renamed under `derive$g$`, which no source can spell. A `where` type is its own `TypeArg` node over its base, carrying the sentence its failure reports; written code calls the declaration's predicate `where$p<Name>` through the `VyrnWp_` placeholder, so the predicate is stated once, in `ctor`.
- The playground runs a generator in the page: `vyrn-genwasm` without `host` builds the module and the `TypeArg` atoms, and `play-wasm.js` runs it. The page serves no read, no module reflection and no code quote, so only a `derive` generator runs there.
- A generator emits code through code quotes (`vyrn"..."`). A string spliced into an expression becomes an escaped literal and into an identifier is validated; there is no way to splice a string as code.
- Code quotes and `lex` exist only during generation and are not reserved words.
- A column layout (one array per field of a record) is `std/columns`, an import-target generator over `moduleInterface`, not a language feature. Its container's `where` rule states that every column has the first one's length, so a loop bounded by one column indexes all of them unchecked. `derive` cannot write it, because its call answers `String` and renames the types it writes. A push rebuilds the container, because a push on one column breaks the rule until the last column grows.
- Generated code maps back to its input through `//@origin path:line:col` lines, which any generator may emit. A diagnostic that cannot be remapped stays at the generated location; it is never dropped.
- A generator reports a diagnostic by writing `//@diag <severity> <anchor> <message>` into its output, so the report survives the cache. Two severities; an unknown word is a warning.
- The compiler knows no lint rule names, codes, registry or suppression syntax.
- A generator symbol map is an exported function inside the generated module, so it cannot go stale against cached code.
- A generator import's identity is its resolved path arguments, so two spellings of one path are one module.
- A JSON Schema type import round-trips byte-exact with the schema emitter; an inexpressible keyword is an error, never a silent drop.

## Standard library and web

- std stays Vyrn. No std function gets a native body, and speed comes from the algorithm (Horspool, SWAR), not from a second implementation.
- One `String` type, with several algorithms behind one function; the cheap path allocates nothing.
- Failure in std is a value: `Result` over small per-operation error enums.
- Floats format through `std/num`'s `f64Str`; text to number is `std/num` over the bit views `floatBits` and `floatFromBits`. No `parseFloat` builtin.
- `std/strings` is ASCII for case and whitespace; `split` on an empty separator returns `[s]`, and `indexOf` returns `Option`.
- `reserve`, `append`, `copyFrom` and `clear` exist on growable `Array` only, and are refused for elements that own heap.
- `m.tally(k, n)` is one probe and never takes the key.
- The runtime never calls a user's hash. It hashes a zeroed canonical pack of the key, so no engine can disagree.
- `std/regex` is a Thompson NFA with no backtracking: the RE2 subset, leftmost-longest, linear time. `=~` stays an anchored compile-time match.
- A `std/regex` search runs the NFA as a lazy DFA over byte-class runs, built per search; past `dfaCap` table entries it falls back to the Thompson simulation, so a search's memory stays bounded and its answers stay the simulation's.
- The JSON codec is canonical: declaration order, no whitespace, `None` fields omitted. `fromJson` returns `Validation`, runs every `where`, parses integers exactly, ignores unknown fields.
- `std/json` is the one strict JSON reader: numbers keep their text, duplicate keys and trailing commas are refused.
- A payload enum crosses the wire externally tagged (`"Unit"`, `{"Circle":5}`, `{"Rect":[2,3]}`), one wire form per value.
- `Map<Int64, V>` crosses the wire as a JSON object with canonical decimal keys; user-keyed maps are refused by the codec.
- VON is Vyrn's record-literal grammar as a data file, read by the compiler's own lexer. It refuses null, interpolation, anchors, environment references and duplicate keys.
- Time and randomness are host inputs: `now()`, `monotonic()` and `randomSeed()` are the only host calls, and the PRNG is a value threaded explicitly. No global `random()`.
- `std/time` is UTC only: no timezone database, `Duration` or date parsing.
- `std/storage` writes atomically (temp file, then rename). `fsyncFile` is opt-in, because an fsync per save costs real latency.
- `std/args` is the minimal argv reader. `std/cli` makes a record type the command: one declaration yields parse and help, and there is no `--version`.
- A lint rule is a library (`std/hints`, `std/vyx-hints`), fires only when it is certain, and is waived by `vyrn-ignore <code>` in the input. No lint framework or `vyrn lint`.
- Typed RPC is a library of generators over an ordinary module. An `Err` return is a 200; 422 is reserved for request validation.
- Each RPC path is derived as `/_/{module}/{name}`. Overrides are data (`rpc.json`), and colliding paths are errors.
- A procedure is serializable only if the compiler's codec can encode its types. A `Stream` is refused by name and pointed at SSE or WebSocket.
- No single service description projected onto every transport. Each protocol has a hand-written `<stem>.<proto>.vyrn` in that protocol's vocabulary.
- Connect, OpenAPI and GraphQL are generator libraries over `moduleInterface`. No gRPC, which would be a runtime project.
- The GraphQL executor is generated beside the SDL from one reflection walk. A resolver's `Err` stays in `data`.
- A client disconnect is detected when a write fails. No keep-alive or ping; a finished producer answers 204.
- WebSocket support is server push only.
- An ETag is FNV-1a-64 of content type and body with no seed, so it survives a restart. Cache-Control is a bare `max-age`.
- `mount` takes the first match. An overlap is a startup trap.
- Streams are pull-based. No `channel` and no arrival-order `merge`, because nothing can push.
- A failing producer yields `Stream<Result<T, E>>`; there is no second error channel.
- The UI layer (`std/html`, `std/ui`, `std/vyx`, `web/`) needs no compiler change, so anyone can build a competing framework from the same parts.
- `std/html` escapes values and checks tag, attribute and event names.
- The DOM diff runs in wasm and crosses as a list of patch operations in a fixed order, so a naive applier is correct.
- `.vyx` templates use one Vue-flavoured grammar: `{{ }}`, `v-if`, `v-for` with a required `:key`, `:attr`, `@event`, `<slot/>`. No `v-model`.
- `std/vyx` names no component. Every capitalized tag is a sibling `.vyx` file or an imported provider, so it is not a framework.
- Pages declare data through `export fn data()` returning `Query`, `Lazy`, `ParamQuery` or `ParamLazy`, and head through `export fn head()`. Laziness is in the type name.
- A layout is the nearest `layout.vyx`; a load failure renders the nearest `error.vyx`.
- The first load is full SSR; navigation renders on the client, and falls back to an HTML swap, then a hard navigation. Soft navigation is progressive enhancement, never a correctness layer.
- Page data is served by content negotiation on the page URL (`Accept: application/json`), and no response carries the string "vyrn".
- `std/tw`'s class vocabulary is closed and checked at compile time: no arbitrary values, no `dark:`.
- Icons resolve at compile time from hash-locked collections, never at run time.

## Tooling

- The workspace builds and tests with no LLVM, clang or sysroot. `vyrn-lsp`, `vyrn-genwasm` and `vyrn-play` sit outside it.
- `vyrn fmt` has one style and no options, never joins or splits lines, and refuses any output whose tokens differ from the input's.
- `vyrn fix` applies only `.copy()`. `consume` and `for x in consume xs` change a contract, so they stay the author's decision.
- `vyrn why --memory`, LSP hover and inlay hints read one ownership table with one wording.
- `vyrn routes` and `vyrn why` read what generators and the source wrote; they recompute nothing.
- `vyrn doc` writes Markdown only. `docs/api/` is committed and CI checks it with `--verify`.
- `vyrn emit-lowered` is deterministic, promises no format and has no parser.
- The LSP is a synchronous pure adapter over the front end. No generator logic is compiled into it.
- The LSP gets no incremental parsing, salsa, incremental sync or delta tokens. Lexing and parsing are under 1% of a keystroke; latency came from repeated pure work, fixed by memoizing it.
- Remote imports are pinned by SHA-256 in `vyrn.lock`, content-addressed in `~/.vyrn/cache`, vendorable and buildable offline. Only `vyrn update` changes a pin. No semver registry.
- A manifest that does not parse is an error, never an empty policy.
- Tools are pinned per project in the same lock, and bytes are shared per user. A pinned tool that cannot resolve fails; it never falls back to `PATH`.
- clang is discovered and recorded, never pinned, because it links against the host's libc.
- Vyrn does not build tools from source, wrap a package manager, or take a crate for curl, git or tar.
- Native builds choose from a curated set of microarchitectures and always pass `-ffp-contract=off`, because FMA changes float bits.
- The editor grammar colours only words the compiler names, and its keywords equal the lexer's.
- A `.vyx` file is analysed through the `.vyrn` module that owns it.
- One `.vsix` serves every platform; the server ships in each platform's release archive.

## Process

- Each change comes with its numbers: lines before and after, refusals lost and gained, benchmark rows.
- A refusal belongs to the kernel or the checker and is pinned with its sentence. A change reports 0 lost and 0 gained, or says why.
- Output parity cannot see memory. Reclamation is gated by the memory suite and by the exit-residue ratchet, whose leak column may only shrink.
- The free audit is a test instrument built only under `VYRN_LEAK_CHECK`, never shipped in a module.
- A change that moves emitted wasm bytes rewrites the SHA-256 manifest in the same commit and names every moved row.
- Censuses of the type constructors and syntax forms are pinned by tests, so the tables cannot drift.
- Pins are written by gates and merged with the `pin` driver, never by hand.
- A test of what `vyrn check` says about one program is `tests/check/<name>.vyrn` and its `.stderr`, not a Rust string.
- A prediction is written as a program before the change lands and reported either way. Nothing is deleted until the corpus is green on its replacement.
- A gate that is missed says so; the bar is never moved quietly.
- A red CI run is a diagnosis. For a change to release timing, the Linux job is the gate, because glibc reuses freed memory differently.
- Pre-1.0 changes ship with no deprecation window: the old form is deleted in the same change.
- No builtin has two peer definitions. An oracle checked by a differential test is allowed.
- A benchmark is a claim about a gap: each deviation gets a named defect or missing feature, and the idiomatic program is the one charted.
- Other contestants are held to their own discipline and built with the flags `vyrn build` passes. Published reference numbers are never divided into ours.
- The benchmark harness runs by hand, not in CI; CI checks that each game program still prints its fixture.
- CI times benches only on pushes to `main`, against a baseline taken on CI hardware, with a 2x threshold. `bench --check` is the blocking half.
- No AI attribution in commits, pull requests, code or prose.
