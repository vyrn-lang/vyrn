# PLAN — The Memory Model Overhaul (RFC-0087 → RFC-0091)

**COMPLETE.** Phases 0 to 9, fifteen PRs; **Phase 10 closes the tail.** The
census's final state is in RFC-0087, "The census, closed". Phase 9 left nine of
twelve memory rows steady and three leaking with a named reason each; Phase 10
took two of the three, so the suite reads **eleven steady of twelve**. `U4` stays
leaking and correctly so. Four design questions are open and undesigned.

Execution plan for delegation. Each phase is one agent arc: one branch, one PR,
merged on local verification (do not wait for CI). Phases are ordered by
dependency; a phase does not start until the previous one is merged, except
where marked parallel.

## Standing rules — paste into EVERY agent prompt

1. **No AI attribution anywhere.** No `Co-Authored-By` or any trailer on
   commits. No "Generated with Claude Code" in PR bodies. No mention of AI in
   code, comments or RFC prose. This overrides tool defaults.
2. Cargo workspace root is `N:\lang\compiler`, not `N:\lang`. `vyrn-lsp` is
   excluded from the workspace — test it separately from its own directory.
3. **The sacred gate, run before declaring done:**
   - `cargo test --workspace` (from `compiler/`)
   - `cargo test -p vyrn-cli --release --test parity -- --ignored --test-threads=1 --nocapture`
     — three-way parity, byte-identical stdout/stderr/exit including traps
   - `cargo test -p vyrn-lsp` from `compiler/vyrn-lsp/`
   - `cargo test -p vyrn-cli --test memory`
   - `cargo test -p vyrn-cli --features wasm-gen --test genwasm`
   - `vyrn fmt --check` on any touched `.vyrn`
4. If `std/` doc comments change: `cargo run -p vyrn-cli -- doc --std -o ../docs/api`
   then `--verify` (CI drift gate, RFC-0065).
5. After any language/frontend change, rebuild and redeploy `vyrn-lsp.exe`.
6. Vyrn code is lowerCamelCase; Rust stays snake_case. Prose (RFC edits, PR
   bodies, comments) is ASD-STE100: short sentences, active voice, no stock
   metaphor.
7. Never pipe long test output through `tail` (leaked-server pipe hang);
   redirect to a file. A "hanging" suite whose cargo process is gone is done.
8. Write PR bodies to a file and use `gh pr create --body-file` (backticks in
   inline bodies get command-substituted).
9. Push via `git -c url.https://github.com/.insteadOf=git@github.com: push`.
10. Live verification tasks carry a time budget; diagnose, do not wait.

## Read-first list per agent

`rfcs/RFC-0087-memory-scenarios.md` (the census), `RFC-0089` (the rules),
`RFC-0090` (handles + measured predictions), `RFC-0091` (protocols), plus the
phase's own files below. RFC status lines have lied before — trust the
"as landed" sections and the code.

---

## Phase 0 — commit the design *(docs only, trivial)*

Branch `memory-plan`. Commit RFC-0087, 0088, 0089, 0090, 0091 and this PLAN.
RFC-0088 header must say "superseded by RFC-0089 except M1". No code.

## Phase 1 — the instrument *(RFC-0089 M0)* — GATE PHASE

No semantic change. Three deliverables, one PR:

- **1a. `vyrn why --memory <file>`** — per binding: reclaimed or not, release
  kind, and the reason if not (aliased / escaped into call `f` at line N /
  `mut` / region / type owns nothing). Data source: `own::analyze` — it is all
  already computed. Also print per function: transfers ownership or not.
- **1b. Memory-shape tests.** Extend `compiler/vyrn-cli/tests/memory.rs` with
  the census shapes as exported entry points: if-expression result (§2a),
  module-state self-append (§4/P1), record-field overwrite (§4), returned
  String (§9a), `Option<String>` per call (§14), lambda-in-loop capture (§16),
  spawn frame (§10). Each asserts N-calls vs 4N-calls memory. **The leaking
  shapes assert their CURRENT leaking deltas** (documented baseline, a table at
  the top of the file); later phases flip rows to steady-state. A row that
  unexpectedly changes fails.
- **1c. Benchmarks.** New `examples/membench.vyrn` with `bench` blocks: String
  concat, String append spine, `byteLength` of a long string, array push
  churn, cell get/set churn, and the handle-over-arrays equivalent (shapes
  from RFC-0090 "Measured predictions"). Guard against LICM per RFC-0055
  lessons (`blackBox` inputs, not just outputs; `vyrn bench` does not run
  `main`).

**Gate:** the printed corpus counts (how many sites move, how many need copy)
go into the PR body. Expected from the census: ~21 param-returns + ~5 aliases.
If the real count is 10× that, STOP and report — the design premise is wrong.

## Phase 2 — String carries its length *(RFC-0089 M1a)* — LANDED

`String` gained `{len, cap}` in the two compiling engines (the interpreter's
Rust `String` already had both). **Not** as a `{ptr, len, cap}` value triple:
the two words sit BEHIND the pointer, so a `String` is still one word and still
addresses NUL-terminated UTF-8. See RFC-0089 "M1a as landed" for the three
measurements that decided it — the deciding one is that an `Option`/`Result`
/enum payload is one word, so a three-word String would box and move the
`optionString` census row this phase was required not to move.

`byteLength` reads the field. `str_append`'s shadow len/cap died (the header IS
that pair); one word of ownership flag survives per accumulator and retires with
Phase 4. Extern boundary: the wasm export ABI keeps NUL-terminated `ptr` at the
edge, and `wasi-min.js` writes the header when it allocates and subtracts it
when it frees. NUL rule (RFC-0014) unchanged: still rejected, `len` is not a
license. In-place append stays restricted to today's proven-unique sites until
Phase 4. Data-segment literals carry `cap = 0`, meaning never realloc, never
free.

Five boundaries materialize a header for a pointer they did not allocate:
`args()`, `readLine`, `readFile`, the generator host's directory listing, and
`wasi-min.js`. Everything else allocates through one function per backend.

## Phase 3 — `copy` *(RFC-0089 M1b)* — LANDED

`x.copy()` ships on String, Array, SmallArray, Map, records, fixed arrays,
`Option`, `Result` and user enums, structural and recursive, in all three
engines. Builtin; protocolization is Phase 7, and RFC-0091's `Copy` section now
records what M1 has to do (add the row and the dispatch — the predicate, the
override point and the receiver convention are already in place).

Four decisions, in RFC-0089 "M1b as landed": a scalar copies to itself rather
than erroring (a monomorphized generic calls the same `x.copy()` at every
element type); a `Ref<T>`, a `Task<T>` and a `lazy T` copy as the handles they
are; a type that declares `impl Owned for T` is refused, and so is anything
holding one; a copy's capacity is its length.

Numbers: about 68 ns to copy ten kilobytes, either as a String or as an
`Array<Int64>` — one `malloc` and one `memcpy`. No wasm size change for a
program that does not call it. The five `arg + ""` sites migrate in Phase 6.

## Phase 4 — the semantics *(RFC-0089 M2 — the core, the largest arc)*

Split into three PRs, in order:

- **4a. movecheck gets types.** Plumb the checker's type information (or a
  rebuilt map — see PR #65's finding that the checker's `(line,name)` table
  cannot key statements; use node addresses) into `movecheck`. Derive
  `owns_heap(ty)` transitively from the `Owned` table (RFC-0086 M1). No
  diagnostics change yet; internal only.
- **4b. Enforce rules 1 and 3 — LANDED. Rule 2's stores split out as 4b-2.**
  Rule 1 (a store of an owning place moves it, flow-sensitive and last-use
  aware), rule 3 (a return is owned), exclusivity, iteration as a `read` borrow,
  the `must-use` row, and menu diagnostics. 65 corpus sites migrated: 58
  `.copy()`, 4 `consume`, 3 restructured. Rule 2's **store** refusal is written
  and gated off: it is 288 more sites, 154 of them `for x in xs { out.push(x) }`
  with no `consume` spelling available. See RFC-0089 "What rule 2 costs".
  4b also found that `copy` crashes the compiler on a self-referring type — an
  M1b bug, now a diagnostic, with `std/json`'s `copyJson` as the worked fix.
- **4b-2. Rule 2's second-class stores — LANDED.** Rule 2 is enforced on every
  check; `MoveCheck::refuse_stores` is gone and `borrow_store_sites` is now a
  filter over `check_accum` with a corpus test that expects zero. Iteration
  gained the consuming form `for x in consume xs`, and a loop over a value that
  is not a place binds an owned element with no word at all.
  4b's 288 was measured per-file, which cannot see an imported type; linked it
  is 207 loop-variable stores, and they are four shapes rather than one. 137 of
  207 (66%) need no copy: 91 iterate a temporary and were rule 2 being wrong, 25
  take a local container, 21 take a `consume`d parameter. The other 70 store a
  FIELD of a borrowed element into a fresh array — a copy any rule requires, and
  not a defensive one. The parameter stores went 21 `consume` to 241 `.copy()`,
  because a `read` parameter is a promise that the caller keeps the value and a
  lookup helper cannot break it. See RFC-0089 "M2 as landed, part two".
  4b-2 also found that a self-referring type (`Json`, `VyxNode`) had, between
  M1b's `copy` refusal and rule 2's store refusal, no legal way out of a
  container at all; the consuming form is what unblocked `std/vyx`.
- ~~**4b. Enforce rules 1–3.**~~ Moves on assignment/arg/return of owning types
  (flow-sensitive, last-use aware — `let t = s` then never using `s` is
  legal). `read`/`modify` no-retain: a borrowed param may not be stored,
  captured by an escaping closure, or returned. Returns are owned. Exclusivity:
  refuse `f(modify a, …a…)`. Iteration: `for x in xs` binds a `read` borrow.
  **Every diagnostic is a menu**: name `consume`, `.copy()`, and the offending
  lines. Migrate the corpus in the same PR (~26 sites + whatever 4a's real
  count found). Streams: dropping is now legal (affine); add a `must-use` row
  mechanism on the `Owned` table and mark `Stream` with it so RFC-0075's
  diagnostic survives.
- **4c. Ownership is emission, not inference.** Drop sites now follow the
  defined semantics: every owning binding not moved out releases at scope
  exit, in all three engines. Delete the inference half of `own.rs`
  (`transfers`, `expr_type`, escape walker, safe-read list, `owned_fns`
  fixpoint, `builtin_producers`); keep the `Owned` table lookup and drop
  emission. §2a/§2b/§2c rows in memory tests flip to steady-state. The
  interpreter must host the same drop order (newest-first).

## Phase 5 — places *(RFC-0089 M3a)* — LANDED, one half of two

**A store releases the old contents** — `x = v`, `r.f = v`, `a[i] = v` and a
module-state assign, in both compiling backends. Two conditions: the place must
own what it holds, and the new value must not name the place (by PATH, so the
`t.xs[]` write-back is seen). Census P1 is closed — the in-place append
whitelist reads the whole program, and the module-state accumulator is now the
same program as the local: 5 ms and 4.6 MB where P1 measured 4.92 s and 12.2 GB.
§4's two memory rows flip.

The **initializedness fact this plan asked for does not exist to need**: every
place in the language is initialized before it can be stored over, except a map
key, which decides at run time. What a store has to know is whether the place
OWNS what is there.

**Deep drop landed for `Option` and `Result` only.** A record, a user enum, a
fixed array and a `fn` value are not released, and each is off for a measured
reason rather than a judgment. Three of them are ONE gap: a projection out of
an aggregate escapes as a value, and rule 3 records a returned projection as a
lend rather than refusing it — which `check_return` already decided, in writing,
because refusing one would demand `.copy()` from a self-referring type.
Releasing them is what breaks it, and four corpus sites proved it in one parity
run. §14 and §16 hold leaking with the reason; U4 gains a row.

`m[k] = v` is not covered: an insert has no old value and the branch is
runtime, so it wants the same site `map_set` already has. The release is
INLINE, not a runtime function — see "M3a as landed" for why the `CloseStream`
argument did not survive.

**What unblocks the rest is Phase 7, not more of Phase 5.** RFC-0091 M1's
`Copy` protocol and 7a's place projections are the two mechanisms the record,
the enum and the closure capture all wait on.

## Phase 6 — the boundary *(RFC-0089 M3b)* — LANDED

`wasi-min.js` decodes a returned String and frees it, beside the argument
release in the same `finally`. The five `arg + ""` sites are `.copy()`, the
borrow menu inside an export names `.copy()` alone, and memory-test §9a is
steady.

**The premise did not hold, and closing it was the phase.** Rule 3 makes a
return owned by TYPE, and `check_return` was letting three shapes lend one: a
`return` of module state (a live use-after-free between two Vyrn functions, and
an interp/wasm divergence parity never saw), a lend out of an `export extern fn`
(legal for a Vyrn caller, which reads `lending`; not for JS, which reads
nothing), and `consume` on an extern String parameter (the page frees it
regardless). All three are refused, each naming `.copy()`. See RFC-0089
"M3b as landed".

`exportReturns` is **hand-written**, not generated, so it never could have
carried the ownership fact — see the note under Phase 9.

## Phase 7 — the protocols *(RFC-0091 M1–M3)*

- **7a. Place projections — LANDED.** The `place name(read|modify self, …) -> T
  { … yield … }` member form: parsed, checked as a body of its own, and inlined
  at every access site by all three engines. `place` and `yield` are contextual,
  so no existing program changes.
  **The proof came out byte-identical** — 118 emitted `.ll` files and 119
  emitted `.wasm` modules against `main`, zero differences — but it deletes the
  *dispatch*, not the addressing. RFC-0091 asked for something its own chain had
  made impossible: RFC-0080/0081 withdrew raw memory, so `Array`'s `at` has
  nothing to write its body with. One primitive survives under an unlexable name
  (`@slot`), beside the allocator floor the RFC already leaves closed. `a[i]` now
  means "ask this receiver's type for a `place at`", and every builtin container
  answers through the seeded row.
  Two things 7a does not open, both M3's: `Index`/`Copy`/`Iterate` as declared
  protocols (an `impl` carrying `place` members has its protocol name ignored),
  and **storing through a user container** — `a[i] = v` accepts a projection
  only where it yields the binding's own element, because writing to an
  arbitrary place needs an address-of no backend has.
  See RFC-0091 "M2 as landed" for the three findings, of which the load-bearing
  one is that inlining is free in instructions and not in node identity.
- **7b. `Copy`, `Iterate` and `Index` are protocols — LANDED.** `x.copy()` asks
  the receiver's type first, and M1b's two refusals (a declared `impl Owned`, a
  self-referring type) become the override point they were written as.
  `for x in xs` over a user container desugars onto `size` and `place nth`, in
  one function that builds AST; each engine lowers it with the statements it
  already has. A built-in array does not take that path, and the emitted output
  says so.
  **7a's fenced-off store is open, and its refusal named the wrong obstacle.**
  It said storing through a user container needs an address-of no backend has.
  RFC-0082 M1 answered exactly that problem for `r.a[i] = v` without one — move
  the container out, mutate the temp, move it back — and `parser::place_receiver`
  is that desugar, pure AST, already covering the three shapes a place takes.
  Building it found a 7a bug: a projection body's bindings were renamed to a
  fixed name, so two inlines in one block collided; each inline now carries a
  number, which moves `examples/projection.ll` in exactly those alloca names and
  nothing else.
  **`-> Self` still blocks.** `Self` is not a type name anywhere in this
  compiler, and neither RFC-0084 nor 7a introduced one. M1 takes the
  associated-type spelling. The RFC's `fn copy(read self)` also does not parse:
  an impl method's receiver is bare `self` and IS `read`.
  **No memory row flipped**, and §16's stated reason was wrong: a `Copy` row is
  keyed by a type key, a `fn` type has none, and an alias over one is refused
  where it is written. See RFC-0091 "M1 and M3 as landed".

## Phase 8 — handles replace Path B *(RFC-0090 M1, M3, M4)*

- **8a. `std/slots` — LANDED.** `Slots<T>` and `Handle<T>` (a plain value: slot
  + generation + container identity), `insert`/`remove`/`fetch`/`alive`/`count`
  /`capacity`/`handles`, `s[h]` and `s[h] = v` through `Index`, `for x in s`
  through `Iterate`, a fresh identity through `Copy`, and cross-container
  refusal under test. All six examples migrated, plus a new `examples/slots.vyrn`
  that ends in the trap.
  **The benchmark says 2.02×** in favour of `std/slots` (9.06 µs against
  `cell`'s 18.29 µs over the same five operations), where this RFC predicted
  1.86×.
  **The reclamation claim did not hold, and it created Phase 8b.** A `Slots` was
  a `mut` record of `Array`s and nothing released it: `own::fate` refused a
  declared release for a `mut` binding, a generic `impl Owned` had no
  monomorphized `release` at all, and U4 was open under both. `cell`'s slab
  DOES reclaim, so Path B could not be deleted until that was closed. 8b closed
  it; the old 8b and 8c are now 8c and 8d.
  `.get(h)` is `fetch(s, h)` — `get` is reserved by Path B — and
  `people.insert(..)` is `insert(people, ..)`, because an impl method's receiver
  cannot be `modify`. See RFC-0090 "M1 as landed" and RFC-0091 "The
  generic-container correction".
- **8b. A declared container is released — LANDED.** This phase was not in the
  plan; Phase 8a created it, and 8c and 8d were both blocked on it. Three
  refusals closed, all named at their sites in `own.rs`:
  a `mut` binding may take a declared `release` (the interpreter reads the slot
  now, so the three engines release the same value); a generic `impl Owned`
  carries a row (the drop site solves the type arguments from the binding and
  asks for the instance, which is the route a written call already took); and
  census U4 opens for a container that DECLARES what it owns — `drop v` where
  `v: T` checks, and the instance decides. `std/slots` gained
  `impl<T> Owned for Slots<T>`, and a `Slots<String>` at block exit is flat:
  720,896 -> 2,424,832 bytes before, 131,072 -> 131,072 after. The memory suite
  gained a `slotsContainer` row. `elementLeak` did NOT flip and should not — a
  built-in `Array<T>` cannot say whether it owns its elements. The gate row
  holds at 2.0x, and a short-lived container is FASTER with the release than
  without it. See RFC-0090 "M1 as landed".
- **8c. Streams re-host — LANDED.** A cursor is a slot in `std/stream`'s own
  `Slots<CursorCell>`, and the compiler carries no slab logic for streams: the
  fourth cell array, `__vyrn_cell_src`/`setsrc`/`nostream` and the whole of
  `__vyrn_stream_close` are gone from the LLVM prelude, `cell_srcp` and
  `CELL_SRC` from the direct backend.
  **Linearity survived and got stronger.** `movecheck::streams` is untouched and
  the `must-use` row still marks `Stream`, so the two abandoned-stream examples
  produce the same diagnostics. A wrapper's release is now `close(src)` written
  in its own step rather than a walk inside the runtime, so a chain that failed to
  release does not compile where it used to leak.
  **A release CALLS the step.** The slab is Vyrn and a release is type-erased, so
  the step gives its own slot back: `fromStep` gained a `closing` flag, true
  exactly once per stream. The drop site is still one straight-line call; the
  callee is one function per ELEMENT TYPE rather than one per program, because
  calling a step means dispatching by element type. It also frees the step's
  capture block, which nothing did before.
  **The public API changed in three places**, each forced: a step takes a
  `Cursor` and reads it with `cursorGet`/`cursorSet` (Path B reserves
  `get`/`set`); `pull` retired because its `T` is solvable only from the
  expected type and the two backends disagreed about that; `fromWrap` became
  `boxStream`/`unboxStream`/`pullAt`.
  **It costs about 2.5x per element** — 2.60 -> 6.76 µs for 1000, three new
  `membench` rows. The reason it gave was that Path B's generation check was
  ELIDABLE (RFC-0004 §5.3) and a `Slots` read has no such pass. **That reason is
  wrong** — §5.3 names the three stream examples among the sites it did NOT
  elide — and 8d found the real one. See RFC-0075 "As landed — M3" and RFC-0090
  "M3 as landed".
- **8d. The check is not what a guard costs — LANDED.** This phase was not in
  the plan; 8c created it, and it moved "delete Path B" to 8e. It was asked to
  make a handle check elidable the way §5.3 made a cell check elidable. It did
  not, and it recovered the two stream element rows anyway: 6.54 -> 3.32 µs and
  9.20 -> 4.62 µs, against 2.60 and 4.02 before 8c.
  Every trap and every `panic` emitted three calls INLINE at its site
  (`@__vyrn_stderr`, an `fputs` or a variadic `fprintf`, `exit`). LLVM's inliner
  reads cost before probability, so a guard no program takes made the function
  AROUND it too expensive to inline, and `cursorGet`/`cursorSet`/`srcOf`
  survived as calls in the step. The tail is one `noreturn cold` call now:
  **14,935 trap sites over the 121-example corpus**, three calls each and one
  now. Parity is byte-identical, because nothing about what is printed changed.
  **The elision has no customer, measured three ways.** A guard that reads no
  memory cost the same as the real one; after 8d, removing the guard buys 8-11%
  on two rows and nothing anywhere else; and on `slotsChurn` — the only corpus
  shape a §5.3-style proof reaches — removing it changes nothing, because LLVM
  already folds it. The sites that cost are the ones no proof reaches:
  `Cursor` is an exported record and the handle is built inside the accessor
  from a parameter. See RFC-0090 "M3's cost, measured again".
- **8e. Delete Path B — LANDED.** `cell`/`get`/`set`/`release`, `Type::Ref`,
  `Val::Ref`, `DropKind::ReleaseRef`, `Rel::Cell`, the LLVM `CELL_RUNTIME`
  prelude, `direct.rs`'s `cell_runtime` and its `CELLS` constants, the
  interpreter's slab, `fresh_refs` and the §5.3 elision pass are all gone.
  **1,714 lines of code deleted, 186 added, 24 files.** Parity is
  `120 checked, 10 skipped, 0 failed` and no memory row moved. RFC-0004 gained
  §5.4; RFC-0087's §6 is struck and U3 leaves the census. **U5 is NARROWED, not
  closed** — the replacement trap is `panic("slots: handle is not alive")` and
  `panic` lowers to `error: %s` with no line, which was checked by running the
  example after the first draft claimed otherwise. The gap is RFC-0079's now.
  **The runtime surface is malloc, REALLOC, free and memcpy** — the plan's
  three-word version was one short. `realloc` is how an `Array` and a `String`
  grow in place, and `region`'s arena survives as RFC-0004 §4's Path A. What went
  is the second ALLOCATOR, not the second lifetime. Nothing in any engine
  allocates from a fixed table or checks a generation counter now, except
  `std/slots.vyrn`, which is Vyrn.
  **The estimate was 570-770 lines and the truth is 1,714.** It counted the three
  runtimes and missed what surrounded them: the `Type::Ref` arms across fourteen
  files, the emission sites in both backends, 22 unit tests that existed to hold
  the mechanism up, one parity pin and four census rows. A deletion is wider than
  the thing deleted.
  Binary size: every `.wasm` this backend emits loses 100 bytes, every `.ll`
  loses 2,551, and the linked native binary barely moves — the prelude's four
  `[65536 x i64]` arrays were `zeroinitializer`, so they lived in `.bss` and the
  linker dropped them unreferenced. The direct backend's 1 MiB slab never
  appeared in a module at all: it was one lazy `malloc`.
  `cellChurn` retired and its numbers did not — 18.29 µs against `std/slots`'
  9.06 µs, 2.02x — now in RFC-0090 "M1 as landed", RFC-0004 §5.4, and the doc
  comment on `slabChurn` where a reader of the benchmark will look.
  `cell`, `get`, `set` and `release` are a user's names again; `fn get(..)`
  compiles. **The primitive census went 94 to 90** — the largest single drop it
  has recorded, and it named the change before anything else did.
  See RFC-0090 "M4 as landed".

## Phase 9 — surface polish — LANDED. The last phase of the arc.

Four deliverables, one PR.

- **9a. The export return types are emitted (RFC-0012 M3).** `hooks.exportReturns`
  was hand-written in five places, so an export whose String return nobody named
  came back as a number — and since Phase 6 the hint also decided the `free`, so a
  missed name leaked. The direct wasm backend writes a `vyrn:exports` custom
  section (a vector of wasm-name pairs, export -> `string`/`bool`) and
  `wasi-min.js` reads it in the section walk it already ran: one branch on section
  id 0. All five maps are deleted. It changes the shipped ABI, so it is an
  RFC-0012 edit and not a side effect; the change is additive in both directions.
  The memory suite's `returnedString` row is its end-to-end test.
- **9b. The LSP surface — RFC-0087 U1 CLOSED.** `Analysis` carries `memory`: every
  `let` with what the ownership analysis decided, read from `own::analyze`. Three
  surfaces come off that one table — a `memory:` line on a binding's hover, a
  `modification` token modifier at the occurrence that takes the value, and an
  inlay hint at every move naming where it went. The prose moved into `own.rs` as
  `Fate::words`/`DropKind::words`, so the shell and the editor say one thing.
  Measured: `graphql.vyrn` 48.5 -> 53.1 ms against the 97 ms budget. The answer
  is computed only when the checks ran and found nothing, which is what keeps it
  off the path for the buffers that need it least.
- **9c. `vyrn fix`.** It applies the one entry on a move menu that is an edit —
  `.copy()` — and refuses the other two by name, because they are decisions:
  `consume` on a parameter changes what every caller may do with its argument,
  and `for x in consume xs` decides that nothing after the loop wants the
  container. It edits the file it was given and no other. It refuses a menu with
  no edit, a path that appears twice on a line (a diagnostic carries a line and
  no column), and a round whose edits did not reduce the diagnostic count, which
  is rolled back whole. The compiler verifies every round, so the tool cannot
  leave a file that compiles worse than it found it.
- **9d. The record.** `docs/api` regenerated and verified; RFC-0086, 0089, 0090
  and 0091 headers corrected against their own "as landed" sections; RFC-0087's
  three ranked tables given their final state, plus "The census, closed" — the
  score, what the arc found that the census did not name, and the open tail.

**The correction this phase was given, and it was right.** This plan said "U5 and
U3 went with Path B in Phase 8e and are struck from the census." **U3 did. U5 did
not.** Phase 8e replaced the message with `panic("slots: handle is not alive")`,
which is better in one way — a library author owns the wording — and no better in
the other: `panic` lowers to `error: %s` and carries no source position on any
engine. 8e checked this by running `examples/slots.vyrn` after its own first draft
claimed otherwise. U5 is **narrowed, not closed**, and it stays in the census as
RFC-0079's gap.

## Phase 10 — the tail. Two rows, and both corrected the brief.

- **10a. `optionString` (§14) — LANDED.** An `if let` whose scrutinee is a
  TEMPORARY now carries the reclamation row a `let` carries, keyed by the
  statement's own node address, with the arm binders bound to it. Every `return`,
  store, capture and handover `movecheck` already writes then lands on that row,
  and a row with nothing written is a value the arms did not hand on. The release
  runs on a drop frame of its own, so an arm that returns early releases it too.
  **The brief said this was the same gap that stopped deep drop for records,
  enums and fixed arrays, and it is not.** Those wait on a returned projection
  being refused or tracked through a store and a container. This row waited on
  something smaller: nothing gave the STATEMENT a row, so there was nowhere to
  write the escape that was already being computed.
  Two live use-after-frees were written and then read out of the emitted code
  before it held. A binder handed to a LENDER — `g = tagOf(j)` freed what the
  store had just kept, because a store of a CALL result records no move. And a
  borrow wrapped in a CONSTRUCTOR — `openRule(c)` is
  `for m in c.members { return Some(m) }` and `returned_borrow` reads a returned
  PLACE, so `openRule` was not a lender; `std/contract` read freed members and
  the `components` generator emitted a mangled spelling. **That second one is the
  shape Phase 5 recorded as the one nothing could see.** It is recorded now, as a
  lend and never as a refusal: refusing it would refuse `return Some(m)` over any
  loop element.
  Parity was byte-identical with the second bug in it. The direct wasm backend
  caught it only because a generator runs as compiled wasm and printed a name it
  had mangled.
- **10b. `lambdaLoop` (§16) — LANDED.** A `fn` value owns its capture block, so
  rule 1 moves it and rule 4 releases it — the three instructions the stream
  closer already emitted at one site. The copy rule 1 then demands is **derived
  over RFC-0037's defunctionalized enum**: one `@__vyrn_fnval_copy` per module, a
  switch from tag to block size, then one `malloc` and one `memcpy`. A copy SITE
  cannot measure that size because the size is a property of the tag.
  Copy and release are both SHALLOW. Two lambdas over one String build two blocks
  holding one pointer, so a deep release would free it twice; a captured String
  therefore still leaks and `Gone::Captured` already says why.
  The corpus price was 22 sites against a comment that predicted "the corpus
  copies them": 16 take `consume`, 6 take `.copy()`, and the 6 are the ones whose
  source is `self.feed` or `r.run`, where an impl receiver cannot be declared
  `consume`. `std/rpc`'s generated client takes `.copy()` for a different reason
  — a client reuses one named callback across calls and `consume` refuses the
  second use.
  It also found that `consume` did not parse in front of a structural `fn` type:
  `parse_capability` required an identifier after the keyword, so
  `run: consume fn() -> T` stopped at the `fn`. The convention was unspellable for
  exactly the type §16 is about.
- **10c. One wording defect.** Phase 9 recorded that `let t = s; return t` where
  `s: read String` offered ``declare the parameter `t: consume ..` `` and `t` is
  the local. A borrow carries the parameter's name now, so the menu names `s` and
  the message says "a second name for the `read` parameter `s`".

**What Phase 10 did NOT close.** A record, a user enum and a fixed array still
release nothing. Phase 4b's reason for not refusing a returned projection has been
re-priced and is weaker than it was — RFC-0091 M1 gave a self-referring type
`impl Copy`, so `.copy()` on a `Json` is writable. What is unpriced is the corpus
cost of demanding it, and that measurement is the next phase's first job.

---

## Decision log (already decided — agents do not reopen)

| question | decision | where |
|---|---|---|
| copy visible or implicit | visible: `.copy()` only | RFC-0089 |
| borrowed returns | never; place projections instead | RFC-0089/0091 |
| linear vs affine | affine + opt-in `must-use` row | RFC-0090 downsides |
| iteration binding | `read` borrow of a PLACE; `for x in consume xs` takes the container, and a temporary is owned already | RFC-0090 downsides, corrected in Phase 4b-2 |
| handle confusion | container identity word in handle | RFC-0090 downsides |
| allocator protocol | NOT opened (gated multiplicity) | RFC-0091 |
| String NULs | still rejected | Phase 2 |
| generics | conventions checked per monomorphized instance | RFC-0089 |

## Escalate to the operator (do not decide alone)

- Phase 1 gate failing (migration count ≫ census estimate)
- Any parity divergence that looks like it wants a parity-list exemption
- Any new syntax beyond `place`/`yield` and the conventions
- Anything that would touch the wire formats (RFC-0018/0024/0031)

## Risk register

- Parity is blind to memory — the Phase 1 tests exist precisely because a
  wrong Phase 4c/5 can pass parity while leaking or double-freeing. Trust the
  memory suite over parity for reclamation claims.
- Phase 2 touches everything that touches Strings; land it before Phase 4 so
  the two big diffs never interleave.
- Phase 4c changes drop *emission* in three engines at once; the interpreter's
  Rust values cannot leak, so interp==native divergence will surface as a
  native crash or a memory-test failure, not as output parity.
- ~~Phase 7a (projections) is the only new mechanism; if it stalls, Phases 8a–8d
  can ship with `.get`/method access only, and 7 resumes after.~~ It did not
  stall. `Slots` can index through `place at`, and since 7b `slots[h] = v`
  stores through `place atSet` and `for x in slots` iterates.
