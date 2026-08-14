# RFC-0101 — A Backend Is an Emitter

- **Status:** **Proposed.** Nothing here is implemented. The measurements are
  real and were taken at `dd3a9fe`; the design is not.
- **Depends on:** RFC-0077 (the direct wasm backend — the second compiled engine,
  and the reason the duplication is now three-way), RFC-0086 (the compiler asks
  the type — "no second list"), RFC-0091 / RFC-0092 / RFC-0093 (the ownership
  model whose release placement is the duplication that has cost the most),
  RFC-0094 (a builtin is a declaration — the same sentence for names).
- **Research:** `docs/research/census-compiler.md`, §1.1 and §5, written on its own
  branch at `7e7aef2`. It is not on `main`. Every number it states that this RFC
  uses was re-measured here at `dd3a9fe`, and two of its claims are corrected
  below: the identity it asks for already exists, and the validation ladder it
  names as duplicated is not.
- **Principle:** the checker decides. A backend encodes what was decided. Where a
  backend decides, it is deciding a second time.

---

## The question

Vyrn has three engines: the tree-walking interpreter (`vyrn-frontend/src/interp.rs`,
9,348 lines), the native backend that writes textual LLVM IR for clang
(`vyrn-codegen/src/lib.rs`, 16,295), and the direct wasm backend
(`vyrn-codegen/src/direct.rs`, 16,180). They agree because a test says they must:
`cargo test -p vyrn-cli --release --test parity -- --ignored`, over 161 examples,
comparing stdout, stderr and exit code byte for byte, traps included.

That invariant is the best thing this project has built. It caught RFC-0077's
whole backend. It is also the reason nobody has had to ask what it costs.

What it costs is that the language's semantics are written down three times, and
the parity suite proves the three copies **agree**. It cannot prove any of them is
right, and RFC-0094 recorded the case: `fromArray` moved its argument, no rule
read the doc string that said so, the native binary exited `0xC0000374`, the
interpreter refcounted its way to the right answer — and parity saw nothing,
because all three engines agreed on the wrong answer.

The question this RFC answers is not "should the backends share code". It is:
**what does a new backend have to write?** Today the answer is "the semantics,
again". Cranelift for fast debug builds, an ARM native target, a second wasm
engine — each of them is priced at a fourth copy of the language. At that price
none of them gets written.

---

## 1. The evidence

Every number below was measured at `dd3a9fe`, in this worktree, by reading the
code. The census took its own at `3ac6d54`; where the two differ, the number
here is the current one.

### 1.1 The two compiled backends share twenty function names and no code

`lib.rs` and `direct.rs` declare 345 and 234 distinct function names. Twenty
names exist in both, as **independent implementations**:

> `call`, `coerce`, `emit_validation`, `expected_fn_sig`, `f`,
> `fn_arg_param_types`, `free_declared_boxes`, `free_snap`, `free_str_temp`,
> `lookup`, `main`, `owns_heap`, `register_fnval`, `resolve`, `resolve_fn_arg`,
> `show_dispatch`, `store_index`, `str_append_shadow`, `str_len`,
> `stream_step_sig`

Strike `f`, `main` and `lookup` as incidental and every remaining name is a
**decision**, not an encoding: which type an expression has, where a value is
released, what a boundary crossing does, which protocol impl a call reaches, how
a closure is registered, what an indexed store means. None of them is an
instruction-selection problem. `owns_heap` is the exception that proves it —
both are thin wrappers over `vyrn_frontend::own::owns_heap` (`lib.rs:3046`,
`direct.rs:9708`), each carrying a comment saying the sharing is the point, and
that pair has never produced a cross-backend bug.

### 1.2 The checker computes every type, validates it, and throws it away

`checker::expr` (`vyrn-frontend/src/checker.rs:3591`) is
`fn expr(&self, expr: &Expr, scope: &Scope, expected: Option<&Type>, fn_ret: Option<&Type>) -> Result<Type, Diagnostic>`.
It derives the static type of every expression in the program and the
substitution of every generic call, with full error checking. It takes `&self`
and `&Program`, so it could not write an answer down if it wanted to.

Four engines then re-derive the same answers:

| Where | What | Size |
|---|---|---|
| `direct.rs:4740` `peek` | a second expression typer for the wasm backend | **510 lines**, 49 call sites |
| `direct.rs:4550` `peek_arm`, `:7613` `peek_ho`, `:5641` `gen_peek`, `:4525`/`:4567` `match_ty`/`join` | its satellites — including the checker's match-arm type join, re-run at lowering time | — |
| `lib.rs` | the `(String, Type)` return convention on every `gen_*` emitter, plus `static_ty` (`lib.rs:3973`, 34 lines) | a convention over ~300 functions |
| `declared.rs:236` `Declared::type_of` | a fourth partial copy, for `own` and `movecheck` | 4-arm |
| `interp.rs:6539` `type_of` | a fifth partial copy | 77 lines |

`solve_param` (`lib.rs:12328`) is documented as "Mirrors the checker's `unify`,
minus error checks (the checker already validated the call)" — it is at least
shared between the two backends, at 12 call sites from `direct.rs`. Contextual
typing is not: `expected_fn_sig`, `fn_arg_param_types` and `resolve_fn_arg` exist
twice, and each backend maintains its own `expect: Vec<Type>` stack.
`direct.rs`'s field comment on that stack says "Same mechanism the LLVM emitter
uses, for the same reason".

`declared.rs:105` records the drift this has already produced: "the two forks had
drifted: the list said `@push` returns `Array<Unit>` and the row says `Array<T>`".

Two smaller consequences of the same discard are worth stating plainly, because
they are visible from outside the compiler:

- **`vyrn_frontend::check` re-parses the source to return the AST it just
  checked** (`vyrn-frontend/src/lib.rs:88`), with the comment "Re-parse to obtain
  it; since diagnostics() reported nothing, lex+parse+check+movecheck all
  succeeded". The checked program is not a value anyone holds.
- **`vyrn check` runs an entire backend to answer one question.**
  `codegen::check_instantiations` (`lib.rs:973`) calls `emit(program)` — the full
  native lowering, producing a complete LLVM module as a `String` — matches its
  error against one needle, `MONO_LIMIT_NEEDLE`, and discards the module. The
  front end has no other way to ask how deep monomorphization goes, because
  monomorphization only exists inside a backend.

The checker already writes down four answers by exception, each added for one
consumer: `check_accum_full` (`checker.rs:313`) returns
`(Vec<Diagnostic>, HashMap<(usize, String), Type>, StoredFnEffects, Vec<Type>, Vec<Type>)`
— diagnostics, the inferred-`let` type table (for the LSP's inlay hints),
RFC-0037's stored-function-value collection (for the `--workers` gate), and the
`toJson` / `fromJson` type lists (for RFC-0078's synthesized codecs). Four
side channels for four consumers, and no side channel for the two consumers that
need the most.

### 1.3 Trap wordings: twenty sentences, ~55 sites, zero shared by three engines

Parity compares stderr. So every trap message is a byte-for-byte contract between
three engines, and **not one of them is held in a place all three can read.**

Sharing stops at a crate boundary. `vyrn-codegen` depends on `vyrn-frontend`, so
`IO_MESSAGES` (8 wordings, `lib.rs:615`), `validation_message` (2 wordings,
`lib.rs:12852`) and `SERVE_STREAM_TRAP` (`lib.rs:11448`) are shared **between the
two backends** and re-spelled by the interpreter — 13 inline sites in `interp.rs`
for `IO_MESSAGES` alone. What the three engines do share is two integers
(`CALL_DEPTH_LIMIT`, `REGION_MAX`), not the sentences they appear in.

Measured: **20 distinct wordings across about 55 literal sites.** The worst is
`array index {i} out of bounds` — six independent `format!`s in `interp.rs`
(`:3596, 3608, 4044, 4247, 4258, 5255`), a seventh for SIMD (`interp.rs:940`), one
`fprintf` format string in `lib.rs:1189`, and a two-piece split in `direct.rs:12612`
because wasm has no varargs. `shift out of range` is written four times inside
the interpreter alone. `out of memory` has six sites across three runtimes,
including the C shim (`toolchain.rs:91, 98, 105`). `call depth exceeds N` has a
fourth copy in `vyrn-play` (`vyrn-play/src/lib.rs:505`).

What holds them together is comment discipline. Fourteen comments say one engine
mirrors another, in both directions:

> `lib.rs:6916` — "(this exactly mirrors the interpreter's `y < 0 || y >= bits`)"
> `lib.rs:1186` — "matching the interpreter's `error: array index {i} out of bounds` byte-for-byte"
> `lib.rs:11446` — "One constant so the two engines cannot drift, which is the rule every trap message in this project follows"
> `interp.rs:4854` — "kept byte-identical to the codegen's format strings so all three backends agree"

The last one is the whole problem in one line: it is a rule, written as a wish, in
a comment, in the file that cannot import the constant.

`interp.rs:93` records what this used to cost before one number was shared:
REGION_MAX "was written eight times across three engines before this constant,
three of those inside string literals".

### 1.4 Release placement: 1,800 lines of walk over 1,421 lines of shared analysis

This is the largest duplication and the one the bug ledger charges for.

`own::analyze` (`own.rs:945`) is shared by all three engines. It returns
`Ownership`, whose `droppable: HashMap<String, HashMap<usize, DropKind>>` answers
one question — **is this node droppable, and nominally how** — keyed by node
address, with `own.rs:76` explaining why an address and not a line. `direct.rs:2986`
says what that buys: "the textual backend reads the same map with the same key, so
the two cannot disagree about which `let` owns what."

That is the whole of what is shared. It is a fact lookup, consumed at **one** site
in the interpreter and **five** in each backend. Everything downstream is written
three times:

| Engine | Release/placement code | Rows pushed onto a scope frame | Exit walks emitted |
|---|---|---|---|
| interp | ~190 lines (`block` 2703, `run_drops` 2784, `release_nested` 2824 — 82 lines) | 1 (`Let` only) | 2 |
| native | ~715 lines (`emit_drop` 4239 — **180**, `deep_release` 3395 — **149**, `release_sum`, `release_enum`, `emit_all_drops`, `emit_drops_above`, `emit_loop_exit_cleanup`, `free_snap`, `free_str_temp`, `free_declared_boxes`) | 5 | 11 |
| wasm | ~895 lines (`rel_at` 2344 — **239**, `emit_rel` 2085 — **108**, `rel_for` 2280 — 64, `rel_each`, `stream_release_at`, `emit_releases_above`, `free_snap`, `free_str_temp`, `free_declared_boxes`) | 5 | 9 |

**1,800 lines of placement against 1,421 lines of analysis** (`own.rs` less its
958-line test module). Three findings inside that:

- **The wasm backend throws the shared answer away.** `rel_for`
  (`direct.rs:2280`) keeps `DropKind` only for two of its seven variants and
  re-switches on `Type` to build a parallel five-variant `Rel` enum. At the `let`
  site (`direct.rs:2992`) it asks `drops.contains_key(..)` — a **boolean** — and
  derives the kind itself. Same at `IfLet`, `ForIn` and `drop x`. The shared
  analysis is being used as a yes/no flag.
- **`droppable` is a `HashMap`, so the order is not shared.** "Innermost frame
  first, newest binding first" is an invariant asserted independently in three
  files: `Gen::drop_stack` (`lib.rs:2049`), `Fn_::releases` (`direct.rs:1436`),
  and a per-block `Vec` (`interp.rs:2718`). The break/continue boundary index is
  reinvented three ways — `LoopCtx::drop_boundary`, the third field of
  `Fn_::loops`, and `Flow::Break | Flow::Continue` propagation.
- **The interpreter acts on 2 of 7 `DropKind`s and 1 of 4 row kinds**, and its
  `?` path runs no drops at all (`interp.rs:2760`), where both backends run a full
  walk (`lib.rs:6629`/`:6675`, `direct.rs:10778`/`:10873`). It is documented as
  intentional — the host reclaims — which means "the three engines run the
  identical plan" is true only for the releases a program can observe.

Agreement is held by eleven comments. They are worth reading as a set, because
they are an enforcement mechanism written in prose:

> `interp.rs:2089` — "The interpreter executes the identical plan, so the three
> engines release the same bindings at the same points."
> `interp.rs:2776` — "Newest binding first, which is the order both compiling
> backends emit."
> `lib.rs:4441` — "A store leaves all three alone rather than making the three
> engines run different programs."
> `direct.rs:2699` — "making both exact means filtering `store_bufs`'s `String`
> entry rather than refusing the whole snapshot, **on both sides at once**."

The last one is an instruction to a future maintainer to edit two files together,
in a doc comment, in one of them.

And the prose has already drifted from the code it describes. `direct.rs:10714`
says `?` "needs no reclamation of its own and cannot leak a frame"; 62 lines
later the same function calls `emit_releases_above` at `direct.rs:10778`, under a
comment saying `?` "owes the same two unwinds. It did not pay them."

### 1.5 The boundary ladder is 505 lines of the same decision

`coerce` is where a value crosses into a declared type: validation, numeric
resize, float crossing, reshape, in an order that is load-bearing.

| Engine | Function | Lines |
|---|---|---|
| interp | `interp.rs:6226` | 161 |
| native | `lib.rs:2364` | 146 |
| wasm | `direct.rs:3784` | 198 |

The ladder is the function; the target vocabulary is its leaves — each decision
emits one to four instructions. `direct.rs:3797` names its referee out loud:
"that is what the interpreter does".

**A correction to the brief this RFC was written from, and to §5 of the census.**
Both said the *validation* ladder — which predicate is checked in what order — is
duplicated. It is not. The min/max/multipleOf/minLength/maxLength/pattern order is
decided exactly once, at import time, in `schema.rs:517`, which flattens a JSON
Schema into a single conjoined `where` predicate; all three engines then walk that
one `Expr` with their ordinary expression evaluator. `emit_validation` is 10 lines
in `lib.rs:11383` and 25 in `direct.rs:4001`, and `validates` is 18 in
`interp.rs:5725`. The predicate *walk* they share (`types::predicate_binds`) even
carries the note "This file had three copies of the walk before RFC-0077 M2d
wanted a fourth; it has none now" (`lib.rs:12869`). Validation is evidence **for**
this RFC's direction, not against it — it is what a shared decision looks like
after it has been shared. The duplication at the boundary is `coerce`, and only
`coerce`.

### 1.6 The bug ledger says where the duplication is expensive

Every cross-backend defect in the recent record lived in the duplicated half.
Diffstats, from the merged commits:

| PR | What it was | `lib.rs` | `direct.rs` | elsewhere |
|---|---|---|---|---|
| #163 (`310753c`) | a map hands back the whole entry | +115 | +95 | `toolchain.rs` +10 |
| #166 (`508d400`) | a match arm hands the payload out | +46 | +34 | fixed **once** in `own.rs` +147 / `movecheck.rs` +166 |
| #172 (`3f4974d`) | the arena's set is the same set on both backends | +11 | +160 | — |

#166 is the shape to read twice. The *rule* was fixed once, in the shared
ownership analysis. It then had to be **consumed** twice, in two backends, in two
different places, and either consumption could have been wrong on its own.

#170 (`855b9d5`) is the same shape without a rule at all: one `HashSet` decided
where the wasm backend put its statics, so three module-state accumulators built
six different wasm modules from one source, per process. The native backend read
the same set for membership only, inside a walk in declaration order — "which is
the asymmetry that kept this quiet". Two consumers of one container, one of which
depended on an ordering the container does not have.

And #165 (`a5e8c4c`) is the exhibit that names this RFC. `mangle_ty` spells
`Option<Int64>` as `OptInt64`, and so does a user type called `OptInt64`. The
native driver dedups its monomorphization worklist on that string, so the second
instantiation was never emitted and both call sites called the first body — a
silent miscompile printing a different number of stack garbage per run, while
`vyrn check` said `ok` and the other two engines printed the right answer. The
direct wasm backend was immune, because it keys its instantiation cache on the
type arguments themselves, and **its comment says why**: a correct description of
a Critical defect in the sibling backend, written as a design note for this one.
One backend documented the other's bug and had no way to fix it.

### 1.7 The proportion

`direct.rs` splits into a driver and extern ABI (~680 lines), types and `Repr`
(~550), the **function lowering** — `Fn_`, `direct.rs:1408` to `:12031`, **10,623
lines** — instruction helpers, and the emitted runtime: allocator, string
intrinsics, `Rt` at `direct.rs:12257` onward, **~3,900 lines**. `lib.rs` has the
same skeleton with the LLVM prelude where the wasm runtime is.

Read any duplicated pair and the split inside it is consistent: roughly two thirds
of the lowering core is decision and one third is emission. Over the whole file
that is **on the order of 40–50% of each backend re-stating semantics that could
be stated once** — 6,000 to 7,000 lines per backend. This is an estimate from
structure and from one bug ledger, not a measurement, and the honest error bar is
wide. Nothing below depends on the exact number; what matters is which half the
bugs are in, and that is not an estimate.

The irreducible target-specific residue is real and large: the shadow stack and
`Repr`, the segregated free-list allocator, the LLVM text plumbing and prelude,
SSA temp management. It is the smaller half, and no recorded cross-backend bug
has lived in it.

One entry that used to be on that list is worth naming, because it is this RFC's
argument already won. Float formatting was 511 hand-written lines per backend
until RFC-0081 M2 replaced both with a call to `std/num`'s `f64Str`, written in
Vyrn (`direct.rs:5911`, `direct.rs:15722`). One definition, three engines, no
wording to keep in step. The same move is available for the decisions above; it
just has nowhere to live yet.

---

## 2. The design

> **The checker's answers become a value. One lowering produces it. A backend
> reads it and encodes it, and decides nothing the lowering already decided.**

### 2.1 What the lowered form is

It is **the checked program with the answers written on it and the sugar gone**.
Not a control-flow graph, not three-address code, not an SSA form. A new crate,
`vyrn-lower`, sitting beside `vyrn-frontend`'s checker and below every consumer:

    source → lex → parse → check → LOWER → { interp | native | wasm | … }

`lower(&Program) -> Lowered` runs after `check_and_synthesize`
(`vyrn-frontend/src/lib.rs:165`), which is already the one place a linked program
becomes a runnable one, and already the only point with both halves of what the
JSON synthesis needs. It is the seam.

`Lowered` holds:

1. **Concrete function bodies, one per instantiation.** Monomorphization moves
   out of the backends. No type parameters survive lowering; `solve_param`,
   both instantiation worklists, the lifted-lambda dedup set and
   `check_instantiations`'s whole-backend round trip all collapse into one
   worklist with one identity — and the identity is the type arguments, not a
   mangled string (#165).
2. **A type on every expression node.** Not a side table — a field. `peek`,
   `static_ty`, both `expect` stacks, `declared::type_of` and `interp::type_of`
   read it instead of deriving it.
3. **Explicit release steps.** `own`'s rows stop being a `HashMap` a backend
   must place and become `Release(place, kind)` steps already in the body, in
   order, at every exit they belong at — every scope exit, every `break`,
   `continue`, `return` and `?`, every match-arm handover. "Innermost frame
   first, newest binding first" becomes the order the steps are in, instead of an
   invariant three files assert.
4. **Resolved trap sites.** A trap carries its wording, already formatted, as a
   string in the form. `IO_MESSAGES`, the bounds messages, the shift and divide
   guards, the depth limits: one table, in `vyrn-lower`, which every engine can
   import because it is below all of them.
5. **Resolved dispatch.** Which `impl` a protocol call reaches, which
   defunctionalized variant a stored `fn` is, which field offset a projection
   names — decided once.
6. **Positions.** Every node keeps the line and column it came from, because a
   trap message and a runtime diagnostic name them.

### 2.2 Control flow stays structured, deliberately

The tempting move is a CFG with basic blocks and branches. That is the wrong
shape here, and the reason is asymmetric: **structured control flow flattens into
a CFG trivially; a CFG needs relooping to become structured, and wasm accepts
nothing else.** `if`, `loop`, `block`, labelled `break` and `continue` are what
`direct.rs` already emits and what `wasm.rs` frames. A CFG in the middle would
make the shared layer pay a relooper so that the one backend that cannot use a
CFG can get its structure back.

So the form keeps `if` / `loop` / `block` / `break n` / `continue n` / `return`,
and Cranelift — which wants a CFG — flattens on the way in. That is the cheap
direction.

Expressions stay a tree, for the same reason: all three consumers already walk one.
An interpreter wants a tree; both emitters emit into a buffer as they walk. Nobody
here needs three-address code, and inventing it would be a fourth traversal to
write and debug before a single duplication is deleted.

### 2.3 What stays in the backend

Everything below the decision:

- **Representation and layout.** `llt` and the LLVM aggregate spelling on one
  side, `Repr` and `layout.rs` on the other, `Val` in the interpreter.
- **Locals.** SSA temps and `alloca` on one side; wasm locals and the shadow
  stack on the other.
- **Encoding, sections, indices, linking**, and the whole of `wasm.rs`.
- **The emitted runtime**: the allocator, the string intrinsics, the prelude, the
  C shim.
- **The ABI**: `extern` import and export shapes, argument widening, the String
  asymmetry RFC-0012 records.

A new backend writes those. It does not write the language.

### 2.4 The interpreter runs it

Yes — and this is the part that changes what parity *means*.

Today the interpreter is the oracle: three engines are compared against each
other, and when they agree, the suite is green. If the interpreter walks the same
lowered form the backends encode, then every decision recorded in the form is
made **once for all three engines**, and parity stops testing whether three copies
agree about it. It tests only what is left: encoding. That is the difference
between an invariant proved empirically over 161 examples and an invariant that
holds by construction over every program.

It is also the phase with the largest cost, which is why it is last (§3, M6).
`interp::expr` is 1,847 lines and `interp::stmt` is 775; they walk `Expr` and
`Stmt` today, and pointing them at `Lowered` is a rewrite of the walk even though
it is not a rewrite of the semantics.

### 2.5 What the census asked for, and what is not needed

The census's §1.1 prescribed "node ids on `Expr`, with a `NodeId -> Type` map and
a per-call-site substitution map", and named node identity through the parser and
through every AST synthesizer as the cost — "the largest single change named in
this census".

**That cost is avoidable, and this RFC does not pay it.** A side table needs an
identity because it is beside the tree. A lowered form does not: the type is *on*
the node, the release is *in* the body, the instantiation *is* the body. Node
identity is a migration concern only — and even there, the repo already has the
identity it would need. `own::analyze`'s rows are already keyed by node address,
consumed by both backends under an explicit comment
(`lib.rs:4546`: "Node-address identity — must match `vyrn_frontend::own`"). The
migration in §3 reuses that key and never generalizes it.

---

## 3. Migration

Six milestones. **The full parity gate is green at the end of every one** —
`cargo test -p vyrn-cli --release --test parity -- --ignored --test-threads=1`,
plus the CI debug profile, plus `memory.rs`, `limits.rs` and `genwasm.rs`. No
milestone lands with a backend half-converted.

Each milestone states a line gate. RFC-0094 M1 demanded a net reduction, measured
+149, and was merged with the bar moved to M2. That is written into RFC-0094 where
it happened, and the same rule applies here: **a milestone that fails its gate says
so in this file.** A gate moved quietly is a gate that was never a gate.

**M1 — the form exists, and it is checked against the copies it will replace.**
`vyrn-lower` produces `Lowered` from a checked program: types on nodes, positions,
nothing else. Nothing consumes it in anger. One new test walks the corpus and
asserts, at every expression, that `peek`'s answer and the native backend's
threaded answer both equal the recorded type. **This deletes nothing on purpose.**
It converts "the two copies agree" from an assumption into a gate, before a line
is removed — and if they disagree anywhere, that disagreement is a bug found by
this milestone rather than a regression caused by M2. Line gate: additive; the
number is the cost of the claim, and it is stated, not excused.

**M2 — monomorphization moves into the lowering.** One worklist, keyed on type
arguments. `lib.rs`'s two mutually-feeding worklists, `direct.rs`'s FIFO index
queue and its lambda dedup set become consumers of a list the lowering hands them.
`check_instantiations` stops running `emit()`. Symbols keep the readable mangle
plus the structural hash #165 gave them, because `emit-ir` output and linker
errors are read for that prefix. Gate: net negative. Two worklists and a
whole-backend round trip go.

**M3 — the backends read types.** `peek` (510), its four satellites, `static_ty`,
both `expect` stacks, both copies of `expected_fn_sig` / `fn_arg_param_types` /
`resolve_fn_arg`, `declared::type_of` and `solve_param`'s backend call sites are
deleted. The `(String, Type)` convention in `lib.rs` becomes `String`. Gate: at
least −1,200 lines across the two backends.

**M4 — release placement.** The lowering emits ordered `Release` steps at the
exits `own` computes, and the backends encode them. The three scope-frame stacks,
the three break/continue boundary indices, `emit_drop` (180), `rel_at` (239),
`deep_release` (149) and `rel_for`'s re-derivation of a kind the shared analysis
already answered all collapse into one placement and three encoders. **This is
the milestone the bug ledger pays for**: #163, #166 and #172 are all placement
drift, and #166 is the exact shape — one rule, fixed once, consumed twice. The
eleven "the other engine does it this way" comments are the acceptance
criterion: each one either becomes unnecessary or names a real target difference.
Gate: at least −900 lines across the three engines. #163, #166 and #172 are the
regression tests; they already exist, in `memory.rs`.

M4 also has a prediction to check first. The interpreter runs no releases on the
`?` path (`interp.rs:2760`) while both backends walk one, and a declared `release`
can print (`interp.rs:2776`). So a program that carries a declaring type across a
`?` should already produce different stderr in the three engines. Either the
corpus never reaches it, or it does and parity is not looking. M4 writes that
program before it writes anything else.

**M5 — traps and the boundary ladder.** One trap table in `vyrn-lower`, below all
three engines, holding the 20 wordings and their conditions. `coerce`'s ladder is
decided in the lowering; the three engines keep only their leaves. The interpreter
stops re-spelling `IO_MESSAGES`. Gate: the count of trap literals outside the table
is **zero**, and a test asserts it — the same shape as the reserved-name gate
RFC-0094 M2 landed.

**M6 — the interpreter runs the lowered form.** Parity becomes structural for
everything M1–M5 moved. Until this lands, the interpreter is still a third copy of
what the form decides, and parity is still doing the work it does today.

**Order, and why.** Types are first because nothing else can move without them: a
release needs a type to know deep from shallow, and a monomorphization is a type
substitution. Release placement is second-to-last among the semantic moves because
it is where the bugs are and it wants the most gate coverage under it. Traps are
late because they are cheap and independent — the one milestone that could be
pulled forward if M3 turns out to be bigger than measured.

---

## 4. What it unlocks

Stated briefly, because none of it is a reason to do M1.

- **A new backend is an emitter.** Cranelift for fast debug builds — the thing
  RFC-0077's own evidence table points at, where clang costs 1,974 ms and
  cranelift 250 ms — becomes representation, locals, encoding and a runtime,
  against a lowered form that already decided the language. An ARM native target
  is the same shape. An alternate wasm engine is smaller still.
- **The differential harnesses cover three engines for one run.** `numbers.rs`
  generates 800 float cases into a Vyrn program and runs it under `vyrn run` —
  one engine, because running the other two costs a toolchain per case. When the
  decisions are in the lowered form, a generated program exercises them once and
  every backend inherits the coverage.
- **The parity suite gets cheaper to keep.** It stays; it just stops being the
  only thing standing between three copies of a trap message.

---

## 5. What this does not promise

- **It is not a correctness argument.** Deciding once means being wrong once, in
  three engines, with parity green — which is exactly RFC-0094's `fromArray`
  defect, and this RFC would not have caught it. Structural agreement removes a
  bug class; it does not add an oracle. The parity suite stays, and the
  differential harnesses matter more after this, not less.
- **It is a new internal contract, and contracts are a tax.** Today a language
  change edits three engines and a test tells you when you missed one. After
  this it edits a form, a lowering, and up to three encoders, and the form's
  shape is a thing to argue about in every RFC that follows. That is a real cost
  and it is permanent.
- **The interpreter loses directness, and the interpreter is used where directness
  pays.** It is the comptime sandbox for `gen fn` (RFC-0021) and it is what runs
  in the playground. M6 puts a monomorphizing lowering in front of both. RFC-0076
  already moved the LSP's generator execution to compiled wasm, so the hot path is
  less exposed than it was — but "less exposed" is not "measured", and M6 must
  measure it before it lands.
- **No compile-speed claim.** The lowering is another pass over the program and
  another allocation of the whole body. It may cost time. Nothing here is
  justified by speed.
- **It does not make a fourth backend free.** The target-specific residue is a
  third of each existing backend — an allocator, a float formatter, a layout
  engine, an encoder — and that is genuinely new work per target.
- **It does not touch the bugs already in the shared half.** `own.rs`'s fates,
  `layout.rs`, `predicate_binds` and the synthesized codecs are shared today and
  can still be wrong.
- **Six milestones is a long arc against a live corpus.** The memory-model arc
  ran ten phases and eighteen PRs, and every phase corrected its own brief. This
  one should be expected to do the same, and the corrections belong in this file.

---

## 6. Open questions

1. **Does `Lowered` own or borrow?** A form that borrows the `Program` is cheap
   and pins the AST for the whole build; one that owns is a second copy of every
   body, per instantiation. M1 should measure the owned version on the largest
   corpus module before the shape is fixed.
2. **What happens to `movecheck` and `own`?** Both key on node address in the AST
   today. If the lowering carries releases explicitly, `own`'s placement output
   has no consumer left in the backends — but `movecheck`'s diagnostics are about
   source the user wrote, and they must not start naming lowered nodes.
3. **Where does the loader's generated code enter?** Generators produce Vyrn
   source that is parsed, checked and lowered like any other module (`loader.rs:1570`),
   so it should need no special case. M1 should prove that rather than assume it.
4. **Can `vyrn-lower` be a module of `vyrn-frontend` instead of a crate?** Lazier,
   and it puts the trap table where the interpreter can already reach it. The
   argument for a crate is only that `vyrn-frontend` is already 13,526 lines of
   checker; that is not much of an argument.
5. **Does the form need a stable text rendering?** `emit-ir` exists for the native
   backend and is read in bug reports. An `emit-lowered` would be the same tool one
   layer up, and it is the cheapest possible debugging aid for M1 — but a rendering
   people read becomes a rendering people depend on.
