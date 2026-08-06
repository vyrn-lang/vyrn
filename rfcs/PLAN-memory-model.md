# PLAN — The Memory Model Overhaul (RFC-0087 → RFC-0091)

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
  **The reclamation claim does not hold, and 8c is blocked on it.** A `Slots` is
  a `mut` record of `Array`s, and nothing releases it: `own::fate` refuses a
  declared release for a `mut` binding, a generic `impl Owned` has no
  monomorphized `release` at all, and U4 is still open under both. `cell`'s slab
  DOES reclaim, so Path B must not be deleted until this is closed.
  `.get(h)` is `fetch(s, h)` — `get` is reserved by Path B — and
  `people.insert(..)` is `insert(people, ..)`, because an impl method's receiver
  cannot be `modify`. See RFC-0090 "M1 as landed" and RFC-0091 "The
  generic-container correction".
- **8b. Streams re-host** their cursor on `std/slots` (RFC-0075 M2c logic
  moves into std).
- **8c. Delete Path B.** `cell/get/set/release`, `Type::Ref`, the LLVM cell
  prelude (158 lines), `direct.rs` `cell_runtime` (212 lines), the
  interpreter slab, `fresh_refs` and the §5.3 elision pass, `DropKind::
  ReleaseRef`. Update RFC-0004 with a "superseded by RFC-0090" section.
  The language's runtime memory surface after this: malloc, free, memcpy.

## Phase 9 — surface polish *(from RFC-0087 Part II, whatever remains)*

LSP: ownership hovers, last-use token modifier, move inlay hints. `vyrn fix`
applying the move-error menu. U5 is gone with Path B; U3 is gone with `cell`.
Regenerate `docs/api`, update memory files and RFC statuses (headers must
match the "as landed" truth — they have lied before).

**Carried from Phase 6: emit the export return types.** `hooks.exportReturns` is
written by hand in four pages and one runtime. An export whose String return
nobody named is decoded as a number and leaks, and since Phase 6 the hint also
decides a `free`. The compiler knows every export's return type. One custom wasm
section (`vyrn:exports`, name -> `"string"` / `"bool"`) read by the shim's own
section walk deletes the map and the class of bug with it. Small, and it changes
the shipped ABI, so it is an RFC-0012 edit rather than a phase's side effect.

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
- ~~Phase 7a (projections) is the only new mechanism; if it stalls, Phases 8a–8c
  can ship with `.get`/method access only, and 7 resumes after.~~ It did not
  stall. `Slots` can index through `place at`, and since 7b `slots[h] = v`
  stores through `place atSet` and `for x in slots` iterates.
