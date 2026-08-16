# RFC-0101 — A Backend Is an Emitter

- **Status:** **Implemented.** Six milestones and twenty-two pull requests — M0's,
  plus the twenty-one the milestone tables below count line by line — and the
  arc ends where §2.3 and §2.4 say it should rather than at zero: the checker's
  answers are recorded and both backends read them, the release ORDER is decided
  once in `own` and consumed at every exit by both compiled engines, every trap
  wording is below all three, the projection expansions (reading and writing
  halves) are expanded once for all three walks, generic and lambda bodies are
  borrowed rather than copied, `where` predicates are the program's own tree, and
  the boundary ladder has a plan with a corpus gate over both compiled engines.
  What is left in a backend is what §2.3 puts there — representation, locals,
  encoding, the ABI — and what an engine deliberately does not run is §2.4's
  five-row list, which is a promise kept rather than a debt.
  **Eight of this RFC's claims died to measurement and each is recorded where it
  stood:** M1's own assertion — that three engines agree about the type of an
  expression — is false, and 22,283 of 570,960 answers say so (§3 M1); §1.4
  priced 568 lines of encoder as placement, so M4's deletion is +35 and not −900
  (§3 M4); M3's −1,200 is permanently unmet, because after every sharing move
  `peek` still answers 109 questions about the backend's own locals (§3 M6b);
  §1.5's "one decision, 505 lines" is two ladders that agree on two rungs and end
  opposite ways (§3 M6b), and its "validation is already shared" is true of the
  order and false of the tree (§3 M6c); §2.4's first draft promised parity would
  hold "by construction" and Miri refuses it (§2.4 [A2]); §6.1's borrow question
  and §6.4's crate question were both answered by measurement rather than by
  design; and the census's §1.1 asked for node ids the AST already had (§2.5).
  **Four defects were found by writing a prediction as a program**, which is the
  method this file recommends and reports either way: a `release` skipped across
  `?`, three construct-owned temporaries the interpreter never released, a dead
  node's type printed by a released `vyrn emit-lowered`, and a call-argument
  temporary the wasm backend leaked inside every lambda body. Two predictions
  came back green and are written up too (§3 M6b, §3 M6d).
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
  Second pass: `docs/research/lowering-design.md`, measured at `1eff3d3` — the
  prior art (rustc MIR, Swift SIL, GHC Core, QBE, Zig AIR, Go SSA, Cranelift,
  MLIR) and the cost of reading this design. Its §4 lists fifteen amendments.
  All fifteen are applied below, and each is marked **[A1]** to **[A15]** where
  it lands, so a reader can check the amendment against the text it changed.
  **[A16] is not one of theirs**: it is the amendment M1 measured for itself,
  and §2.1 item 2 is where it lands.
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
| `lib.rs` | the `(String, Type)` return convention on the `gen_*` emitters, plus `static_ty` (`lib.rs:3973`, 34 lines) | **16 functions, 40 occurrences** |
| `declared.rs:236` `Declared::type_of` | a fourth partial copy, for `own` and `movecheck` | 4-arm |
| `interp.rs:6539` `type_of` | a fifth partial copy | 77 lines |

**[A10] That row said "a convention over ~300 functions" and it was wrong.**
Measured in `docs/research/lowering-design.md` §2.3: **16 functions return
`Result<(String, Type), String>`**, the spelling appears **40 times**, and
`lib.rs` holds 400 `fn`s. The first draft counted the file, not the convention.
M3's gate is written from the measured number below, because a gate written from
a wrong number is a gate nobody can meet or miss on purpose. The convention is
smaller than claimed here and larger than claimed on the other axis: `peek`'s
49 call sites are verified exactly.

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

**This table adds up to one number and two functions, and M6's first phase
measured which.** The two compiled ladders take a type PAIR; the interpreter's
takes a value and a target and has no `from` at all, because it walks an untyped
`Val`. So the 505 is 344 lines of one decision and 161 of a different one, and a
shared plan can serve two engines before M6 and three after it. §2.4's list row 4
is where that lives, and M6's third phase is where the plan and its gate landed —
four named differences, all of them ORDER, and zero the rules do not explain.

**And the 344 is not one decision either — M6's second phase read the two
compiled ladders rung by rung before writing a shadow over them.** They agree on
the first two rungs, order the middle differently (native reshapes arrays before
it converts numbers; wasm converts numbers before an `ll`-equality shortcut the
other has no equivalent of), each has a rung the other lacks, the RFC-0020
containment skip sits in a wrapper on one side and inside the validation rung on
the other, and the ladders END differently: native falls through to identity,
wasm refuses. §3 M6's second phase has the table and the shape a shadow has to
take because of it.

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

**True of the ORDER, and false of the TREE the order runs over, which M6's second
phase measured while looking for something else.** `types::decl_map` copies every
`TypeDecl` into a map of its own, once per engine, and each validation site then
copies the predicate again to get past the borrow checker
(`lib.rs:11655`, `direct.rs:4169`). So all three engines walk the one `Expr`
`schema.rs` decided — as three copies of it, at three sets of addresses, at every
value boundary a refined type crosses. **1,043 of the corpus's 2,035 off-program
backend answers are inside one**, which is twice the lifted lambda's clone and
the largest single class of it. Nothing about the decision drifts; what the
copies cost is that no side table keyed by node address can reach a predicate.

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

**[A1] This shape is not novel, and naming what it copies is worth more than
arguing it.** It is Zig's AIR: fully typed, one instance per instantiated
function, produced after comptime and generics are resolved, structured control
flow as instructions, and six backends over it including a C backend that emits
text (`src/Air.zig`, `src/codegen.zig`; see `docs/research/lowering-design.md`
§1.5). Every structural choice in §2.1 and §2.2 was reached here independently
and is shipped there. Zig is also the only surveyed system with no runtime
generics and no garbage collector, which is Vyrn's position exactly. An RFC that
proposes a novel shape is argued. An RFC that proposes a shipped shape is
checked — and a future reader who finds this form wrong knows where to look.

`Lowered` holds:

1. **Concrete function bodies, one per instantiation.** Monomorphization moves
   out of the backends. No type parameters survive lowering; `solve_param`,
   both instantiation worklists, the lifted-lambda dedup set and
   `check_instantiations`'s whole-backend round trip all collapse into one
   worklist with one identity — and the identity is the type arguments, not a
   mangled string (#165).

   **[A3] This RFC cited rustc for this item, and rustc does the opposite.**
   `TyCtxt::instance_mir` is keyed on `DefId`, not on generic arguments: the
   shared MIR body is generic, and each consumer substitutes at the use site —
   codegen with `instantiate_mir_and_normalize_erasing_regions`, the interpreter
   with `instantiate_from_current_frame_and_normalize_erasing_regions`. One body,
   two instantiation strategies. Zig does it the other way: `InternPool` gives
   each generic instantiation its own function entity, and no backend ever sees a
   generic. **This is the one place the two strongest precedents disagree, so
   this RFC chooses rather than cites.**

   **Choose Zig's.** rustc keeps a generic body because it must — trait objects,
   separate compilation, an interpreter that instantiates lazily. Vyrn has none
   of those, monomorphizes everything already, and has no function pointers
   (RFC-0037; `direct.rs:864`). A substitution is a decision, and a generic body
   leaves that decision in three engines, which is the sentence this RFC is
   built on.

   **The fallback is rustc's, and it has a threshold. M1 measured it, so the
   threshold is now a number.** The largest corpus module is `graphql.vyrn` —
   372 linked functions, **368 instances**, 15,867 rows. The borrowed form M1
   ships holds **1.69 MiB** live (2.79 MiB peak, 52 ms). A concrete `Block` per
   instantiation on top of that is **+1.20 MiB** live (3.6 ms) — **1.71×**, or
   3.4 KiB per instance. Method: a counting global allocator, live bytes held
   while the value is alive, in `vyrn-cli/tests/lowered_cost.rs`, which states
   its own limits — heap held, not resident set, so it excludes the shared AST
   and the allocator's slack. It is reproducible on every platform, which peak
   RSS on Windows is not, and that is the trade this number makes.

   **The threshold, written from that: switch to the generic-body fallback when
   the lowered form of one module holds more than 256 MiB live** — about ninety
   times the largest thing the corpus builds today. Nothing under that is worth
   a second substitution mechanism. M2 is the first milestone that can cross it,
   because M2 is where the instantiation count stops being M1's partial one.
2. **A type on every expression node.** Not a side table — a field. `peek`,
   `static_ty`, both `expect` stacks, `declared::type_of` and `interp::type_of`
   read it instead of deriving it.

   **[A16] M1 measured this item and it is under-specified: a node needs TWO
   types, not one.** The checker's answer at a node is the type the value must
   END UP as — `Expr::Int` under an `Int32` destination is `Int32`. A backend's
   answer is the type the value HAS when the node's code has run, before the
   `coerce` that follows — `Int64`, every time. Those are different questions and
   neither engine is wrong. Over the corpus this single difference is 21,140 of
   the 22,283 disagreements M1 found (§3 M1). So M3 cannot delete `peek` by
   handing it the checker's contextual answer: the form must carry the node's own
   type and the type its context requires, and the pair of them is what makes
   `coerce`'s 505-line ladder (§1.5) a decision the lowering can own in M5.

   **Shipped in M3a, and the amendment needed one of its own: the checker cannot
   answer the has-question, so the FORM derives it.** This item says `peek` reads
   the type "instead of deriving it", and [A16] then asked the form to carry the
   node's own type — but `Checker::expr` types every expression against its
   destination and holds no second answer to read out. So `Row::has` is derived
   in `vyrn-lower`, from the node's own shape and its children's, in a closed
   table of nine arms (`has_of`). That is a derivation the lowering performs, and
   the module doc that said it derives nothing is corrected where it stood: it is
   the derivation `peek` and `static_ty` ARE, written once below both backends
   instead of twice inside them, which is this RFC's sentence rather than an
   exception to it. `Row::ty` is still the checker's own answer, unchanged.
   Measured: the pair explains **21,154 of the 22,321** answers that differ from
   the destination type (§3 M3).
3. **Explicit release steps.** `own`'s rows stop being a `HashMap` a backend
   must place and become `Release(place, kind)` steps already in the body, in
   order, at every exit they belong at — every scope exit, every `break`,
   `continue`, `return` and `?`, every match-arm handover. "Innermost frame
   first, newest binding first" becomes the order the steps are in, instead of an
   invariant three files assert.

   **The releases are elaborated before any backend sees the body, and two
   compilers reached that independently.** rustc's `MirPhase` says what the
   difference is: in analysis MIR a `Drop` terminator is a *conditional* drop; in
   runtime MIR "the drops are unconditional", and `ElaborateDrops` is the pass
   between them, classifying each drop as static, dead, conditional or open from
   a maybe-initialized dataflow pair. **An unelaborated drop is a question. An
   elaborated drop is an instruction.** Vyrn's `droppable` map is the question,
   asked once in the interpreter and five times in each backend, and each asker
   answers it again. Swift makes the same move from the other side: `destroy_value`
   is an instruction in the IR, the `SILVerifier` checks it at every point until
   ownership is lowered, and the pass that erases it has been moved *later* over
   time as more passes learned to keep it. So the direction of travel in both
   systems is to keep releases explicit longer, not shorter. This item is the
   elaborated form, in the shared body, before the split.
4. **Resolved trap sites.** A trap carries its wording, already formatted, as a
   string in the form. `IO_MESSAGES`, the bounds messages, the shift and divide
   guards, the depth limits: one table, in `vyrn-lower`, which every engine can
   import because it is below all of them.
5. **Resolved dispatch.** Which `impl` a protocol call reaches, which
   defunctionalized variant a stored `fn` is, which field offset a projection
   names — decided once.
6. **Positions.** Every node keeps the **line** it came from, because a trap
   message and a runtime diagnostic name it.

   **[A4] This item said "line and column", and the AST has no column to carry.**
   `compiler/vyrn-frontend/src/ast.rs` spells `line: usize` 40 times and
   `col: usize` twice. `Diagnostic` carries `col` and `end_col`
   (`diagnostics.rs:52-56`), but the tree the lowering reads does not. Promising
   columns on every node means threading a column through the parser and through
   every AST synthesizer — the pervasive change §2.5 says this design avoids. No
   trap message prints a column. So the form carries the line, and a consumer
   that later needs columns writes its own RFC and pays the parser cost there.

### 2.2 Control flow stays structured, deliberately

The tempting move is a CFG with basic blocks and branches. That is the wrong
shape here, and the reason is asymmetric: **structured control flow flattens into
a CFG trivially; a CFG needs relooping to become structured, and wasm accepts
nothing else.** `if`, `loop`, `block`, labelled `break` and `continue` are what
`direct.rs` already emits and what `wasm.rs` frames. A CFG in the middle would
make the shared layer pay a relooper so that the one backend that cannot use a
CFG can get its structure back.

So the form keeps `if` / `loop` / `block` / `break n` / `continue n` / `return`,
and a Cranelift backend — which wants a CFG — flattens on the way in. That is the
cheap direction, and Zig's AIR is the same choice made for the same reason: it
is the only surveyed system with structured control flow in its shared form, and
the only one with a first-class wasm backend of its own.

**[A15] An earlier draft said Cranelift "flattens on the way in" as if Cranelift
did the flattening. It does not.** `cranelift-frontend`'s `FunctionBuilder`
builds SSA for the producer — `declare_var` / `def_var` / `use_var` over Braun et
al. — but Cranelift IR is a flat CFG with no structured `if` or `loop` in the
builder API, and **the producer writes the flattener**. Wasmtime's own wasm
frontend carries a `ControlStackFrame` stack to do it. So a Cranelift emitter
starts with a control-flow flattener before it emits one instruction. The
direction is still the cheap one — a relooper in the shared layer would be worse,
and every backend would pay it — but the flattening is work, and §4 must not
price a new backend as if it were free.

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

**[A5] And the rule that was here by omission is now a rule: the form is the
contract, and there is no shared emitter interface.** No `BuilderMethods`, no
`emit_add` trait, no abstract instruction builder that three backends implement.
rustc built exactly that — `rustc_codegen_ssa`'s `BuilderMethods` — and its
newest backend declined it: a code search for `BuilderMethods` under
`compiler/rustc_codegen_cranelift` returns **zero hits**, while
`rustc_codegen_llvm` and `rustc_codegen_gcc` both implement it across
`builder.rs`, `abi.rs` and `intrinsic.rs`. cg_clif reuses the driver and the
linking layer, then walks MIR itself with its own place, discriminant, vtable,
ABI and intrinsic code. **Two of three backends share the instruction
abstraction. Three of three share the IR.** An omission is not a rule, and the
next person to propose a shared emitter trait should have to argue against this
paragraph.

### 2.4 The interpreter runs it

Yes — and this is the part that changes what parity *means*.

Today the interpreter is the oracle: three engines are compared against each
other, and when they agree, the suite is green. If the interpreter walks the same
lowered form the backends encode, then every decision recorded in the form is
made **once**, and no engine can hold a different answer to it by accident.

**[A2] The first draft claimed more than that, and the claim does not survive its
own §1.4.** It said parity "stops testing whether three copies agree" and that
the invariant would "hold by construction over every program". Two facts refuse
it. Miri runs the same MIR the backends compile — `Machine::load_mir` and
`codegen_mir` both go through `TyCtxt::instance_mir` — **and** disables five
passes over it, because it wants its own diagnostics
(`MIRI_DEFAULT_ARGS`: `-CheckAlignment,-CheckNull,-CheckEnums`,
`-ReferencePropagation`, `-GVN`). A shared form did not make those two consumers
identical. And Vyrn's interpreter already does the same thing: it runs no
releases on the `?` path (`interp.rs:2760`) because the host reclaims, which
§1.4 records as intentional. The form does not delete that difference. It moves
it.

**The smaller true claim: a shared form does not make three engines identical. It
makes their differences declared instead of accidental.** Today a difference
between the engines is a comment, or nothing, and parity finds it as a symptom
over 161 examples. After this, a difference is a place where an engine
deliberately does not run what the form says, and there are few enough of them to
list. Parity keeps its job on everything else — encoding — and it keeps its job
on the declared differences too. That is a smaller promise, it is true, and it
still justifies six milestones.

It is also the phase with the largest cost, which is why it is last (§3, M6).
`interp::expr` is 1,847 lines and `interp::stmt` is 775; they walk `Expr` and
`Stmt` today, and pointing them at `Lowered` is a rewrite of the walk even though
it is not a rewrite of the semantics.

**THE LIST. Every place an engine deliberately does not run what the form says,
in one place, because "few enough of them to list" is a promise and this is the
list.** M6's first phase writes it; a milestone that adds a difference adds a row
here, and a difference NOT here is a bug rather than a decision — which is the
whole of what the paragraph above promises.

| # | The engine | What it does instead | Why | What holds it |
|---|---|---|---|---|
| 1 | interp | runs 2 of `own`'s 7 `DropKind`s | a `String` or an array buffer is reclaimed by the host at exit; the two it runs are the two a program can observe — a declared `release` and the walk that reaches one | `RelRule::HostReclaims` in `vyrn-cli/tests/lowered.rs`, a **subsequence** test: this engine may run fewer steps, never a different order |
| 2 | interp | derives its own release ORDER rather than reading the placement | `run_drops` looks a binding's value up by name across the frames being popped, which is what `frozen` disambiguates; consuming the placement there is a rewrite of the unwinding for a deletion of one line (M4d's ledger) | the same gate, plus the nine per-exit-kind fixtures |
| 3 | interp | never reaches `RelRule::Terminated` or `RelRule::StreamCursor` | a stream cursor is frame structure both compiled backends splice and this engine has no frame to splice it into (M4d, finding 1) | both rules were deleted rather than left to fire zero times |
| 4 | interp | reconciles a boundary from the VALUE, not from a type pair | `Interp::coerce(v, ty)` has no `from` argument at all: it matches on the target type and the runtime `Val`. The two compiled backends match on `(from, to)`. §1.5 priced one ladder across three engines and it is two ladders — a shared plan can serve two of the three, and the third can consume one only once it runs the form, which is this milestone | measured in M6's first phase; the two compiled ladders are gated against `coerce_plan` by the corpus gate since M6's third phase, and this engine is the one of the three the plan cannot serve until it runs the form |
| 5 | interp | does not monomorphize | it substitutes at the call and walks the one generic body; §2.1 item 1 chose Zig's instantiation for the engines that EMIT | the instance-list half of the corpus gate compares the lowering against the two backends only |

**Two things are deliberately NOT on this list.** The `?` path — §1.4 recorded it
as an intentional difference and M4's step 0 proved it was a defect, so it is
fixed and pinned by `examples/releaseacrosstry.vyrn` rather than declared. And
trap wordings — M5b put every one of them below all three engines, and
`vyrn-cli/tests/traps.rs` asserts nobody re-spells one.

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

### 2.6 The form checks itself, in debug builds, forever

**[A7]** A form with a type on every node admits an independent type-check over
itself: walk `Lowered`, re-derive each node's type from its children, and assert
it equals the recorded one. That is GHC's `-dcore-lint`, described by GHC's own
authors as "a 100% independent check on the type inference engine", and it is
what Swift's `SILVerifier` does for ownership until ownership is lowered. **Both
systems that verify their IR verify it continuously.** An untyped IR cannot buy
this at any price.

M1's corpus gate is a one-off version of the same check, aimed at the copies it
will replace. Making it permanent — `debug_assert`-gated, one pass, off in
release — turns "the lowering is right" from a claim a migration made once into
an invariant every future change meets. It also gives the release steps of item 3
somewhere to be checked: a release of a place that is not live is a lint failure,
not a leak found by `memory.rs` three PRs later.

**Shipped in M1, and it paid immediately.** `vyrn_lower::lint` is one function,
called by a `debug_assert` inside `lower` and by the corpus gate over every
example. It found both of the recording's gaps — the unsolved parameter left on
a generic call's argument, and the same gap in a generic record literal — before
either reached a reviewer. What it does NOT do is re-derive a node's type from
its children: that needs a second type derivation, and having one derivation is
what this RFC is for. It checks that the answers are there, that an instance is
concrete, and that the order is one a dump can be diffed in.

### 2.7 The form has a text rendering, and it is `vyrn emit-lowered`

**[A11] Not an open question — an M1 deliverable.** Open question 5 asked
whether the form needs a text rendering. It does, it is cheap, and the thing that
makes it safe is a gate rather than a promise.

**The name is a subcommand.** This repository spells emitters as subcommands —
`vyrn emit-ir`, `vyrn emit-gen` (`vyrn-cli/src/main.rs:6-7`) — not as build
flags. So `vyrn emit-lowered <file>`, beside them, and not `build
--emit-lowered`.

**Scope: the root module's functions, by default.** `vyrn why --memory` already
decided this and wrote down why: "Only the file asked about. A linked program
carries every import's functions, and they are another file's answer"
(`main.rs:1139-1141`). The median example is 67 lines. Its linked program is not.

**Shape: one decision per line, indentation is structure, the position is the
last column.** A type on every binding and every call result, so `grep ': Array<'`
answers a class of question in one command. A trap site is one token — `!aoob` —
so `grep '!'` lists every trap a program can reach. A release is a line, not an
inference: `release xs : FreeArr @8 exit=fn`, which is `own`'s answer *placed*.
Instantiations are spelled as `fn map<Int64,Str>`; the mangled symbol stays in
the emitters, because #165 was a mangled string used as an identity and a dump
that shows the mangle invites the same confusion into every bug report.

**The rule the dump exists to serve: a decision lives in one file, and the dump
names that decision at every site it applies.** `docs/research/lowering-design.md`
§2.2 measured what that is worth on one question — *where does an array bound get
checked?* Today, answering it across the three engines means reading **23 sites
in three files, over 700 lines**, none of which references any other except
through a comment. After the form, it is `vyrn emit-lowered f.vyrn | grep '!aoob'`
plus one arm of the lowering. Where a decision has to be re-stated per target it
is not a decision, and §2.3 already lists what is allowed to be re-stated.

**Determinism is a gate, not an intention, and the gate lands with the dump.**
This repository has already paid for the other choice: one `HashSet`, iterated by
the direct backend, built "SIX different modules from this one file — same
length, first difference at byte 1016" (`tests/reproducible.rs:8-17`), and that
test crosses a process boundary on purpose because a `HashSet` iterates
identically twice inside one process. `emit-lowered` gets a row in that file on
the day it lands, beside `the_same_source_emits_the_same_ir_in_every_process`.
Sort by module, then name, then rendered type arguments. Never print from a
`HashMap`.

**And the gate is the whole point, because the precedent this design copies got
this part wrong.** Zig's `--verbose-air` is debug-only, checked by nothing, and
has been incomplete or crashing repeatedly (ziglang/zig #7670, #10031, #12599).
This repository has the matching lesson at `b1eef04`, where a second native
backend was deleted after going from working to unbuildable in twelve days,
unnoticed, because nothing checked it. An ungated dump decays, and a decayed dump
is worse than none, because a reader trusts it.

**Shipped in M1, minus one line of the sketch.** A trap site is not a token yet:
there is no trap table until M5, so `!aoob` has nothing to name and the dump does
not invent one. Everything else is there — a type on every binding and every call
result, instantiations spelled (`fn id<Int64>` and `fn id<String>`, two entries
for one function), positions in the last column, root module only. It ships with
its row in `tests/reproducible.rs` and two blessed snapshots.

One thing the dump also repairs. `vyrn emit-ir` gives the native backend a text
tier that ten test files already use to prove properties no program output can
show — `tests/places.rs` counts allocating calls in the emitted IR to prove a
container mutation moves a header instead of copying it. **The direct wasm
backend has no text form at all.** `emit-lowered` sits above both and covers the
half of the compiled surface that is currently untestable that way.

---

## 3. Migration

Six milestones, **twelve pull requests**. **The full parity gate is green at the
end of every one** —
`cargo test -p vyrn-cli --release --test parity -- --ignored --test-threads=1`,
plus the CI debug profile, plus `memory.rs`, `limits.rs` and `genwasm.rs`. No
milestone lands with a backend half-converted.

Each milestone states a line gate. RFC-0094 M1 demanded a net reduction, measured
+149, and was merged with the bar moved to M2. That is written into RFC-0094 where
it happened, and the same rule applies here: **a milestone that fails its gate says
so in this file.** A gate moved quietly is a gate that was never a gate. A
twelve-PR arc needs that discipline more than a six-PR one, not less.

### 3.0 Two PRs per milestone, and the budget each has to fit

**[A8] The first draft's six milestones did not fit what one agent can land.**
M3 deletes 510 lines and rewires 49 call sites; M4 rewrites three encoders
together and is the milestone the bug ledger pays for; M6 points 2,622 lines of
interpreter walk at a new form. Measured over the last 30 commits on `main`, the
median commit is **611 insertions across 10 files**. That is the proven budget,
so it is the budget: **≤ 800 changed lines and ≤ 15 files per PR.**

M1 already invented the pattern that fixes this — "this deletes nothing on
purpose" — and applied it once. It is now the rule for every milestone. Each
lands as two PRs:

- **A — shadow.** The lowering computes the answer. A corpus gate asserts it
  equals what each engine derives today. Nothing is deleted, nothing changes
  behaviour, parity is trivially green, and a disagreement is a bug this PR
  *found* rather than a regression the next PR *caused*. Additive, small, and its
  line count is stated rather than excused.
- **B — delete.** The engines read the recorded answer and their derivation goes.
  Nearly all deletion. A deleted line is the cheapest line an agent can review.

The risky half of each pair is the small additive one. The line gates below are
the gates for the pair; PR A is expected to be positive and PR B pays for it.

**M0 — the failure output, before any of this.** [A14] When a parity run goes
red, `tests/parity.rs:118-123` and `:167-172` push two whole program outputs,
`{:?}`-escaped onto one line each, with every newline spelled `\n` — for a corpus
whose largest example is 944 lines. The reader must find the first difference by
eye. The same repository already knows better: `reproducible.rs` reports the
offset of the first differing byte, and `limits.rs` reads the number it asserts
out of the code that enforces it. Replace the two dumps with the first differing
line number, that line from each engine, and two lines of context. **This is one
function, it needs nothing from this RFC, and it is the failure output every
milestone below will be read through. It lands in its own PR ahead of M1.**

**M1 — the form exists, and it is checked against the copies it will replace.
IMPLEMENTED.**

`vyrn-lower` produces `Lowered` from a checked program: types on nodes, lines,
nothing else. Nothing consumes it in anger. **This deletes nothing on purpose.**

**Four pull requests, not one, and each one's changed-line count against §3.0's
≤ 800 / ≤ 15 files:**

| PR | What | Changed lines | Files |
|---|---|---|---|
| M1a | the checker records; both backends say their answer out loud; the monomorphization bound moves below both | 329 | 4 |
| M1b | the `vyrn-lower` crate: the form and its lint | 703 | 4 |
| M1c | the corpus gate | 521 | 2 |
| M1d | `vyrn emit-lowered`, its two gates, the measurements, this text | 795 | 10 |

The budget held for all four. M1 is additive by design, so the number is the
cost of the claim, stated rather than excused: **2,273 lines added and 75
removed — 2,348 changed across 20 files.** Roughly a third of it is the two
gates and their fixtures, and none of it is deleted yet; M3 is the first
milestone with a negative gate.

### What M1 found, which is not what it went looking for

**M1's own assertion is false.** It said the corpus gate would assert, at every
expression, that `peek`'s answer and the native backend's threaded answer both
equal the recorded type. Measured over 138 linked corpus programs and **570,960
backend answers**: **22,283 differ from the checker's answer, and 3,383 differ
between the two BACKENDS.** No program notices, because every one of them is
coerced immediately afterwards — which is exactly why the parity suite has never
seen any of it. Parity compares output; it cannot see a type.

That is a larger result than "they agree", and it is what a shadow PR is for.
The differences are not noise: they are five structural facts, and the gate that
shipped is that **every difference falls under a rule named in the test**, with
an unexplained one failing the run. So a new class of disagreement is caught
while these are recorded:

| Rule | Count | What it is |
|---|---|---|
| `SameAfterResolve` | 376 | `MaybeAge` against `Option<Int64>`, `Age` against `Int64`, `User` against its record shape — each engine resolves a declared name at a different point. |
| `DefaultedPosition` | 21,140 | The literal `1` under an `Int32` destination, the element type of `[]`, the unused side of a `Result`: one side wrote its default where nothing constrained the position. This is item 2's amendment [A16], and it is the class by two orders of magnitude. |
| `ArrayShape` | 3,419 | `Array<E>` against `Array<E, N>` or `SmallArray<E, N>` — the literal's own type against the type it is stored as. |
| `LessSpecific` | 677 | One side kept a type parameter, or dropped a generic's arguments (`Crate` for `Crate<Cargo>`). **The class M3 deletes rather than reconciles.** |
| `Diverges` | 55 | A `match` whose every arm leaves the function: the backends type it `Never`, the checker types it as the destination. Both are right about a value that is never produced. |

**Nothing here is a miscompile, and M1 found no bug in the emitted code.** What
it found is that the three engines have never been asked the same question, and
that M3's gate — "delete `peek`, read the recorded type" — needs item 2's second
type before it can be met.

### Where the design did not survive contact with the code

Seven, each recorded rather than smoothed over.

1. **`peek` does not run at every expression.** It runs at joins — 49 call sites,
   which §1.2 measured correctly and the M1 text then forgot. The wasm backend's
   per-expression choke point is `Fn_::expr` (`direct.rs:4156`), so the gate
   observes three sites, not two: `Gen::gen_expr`, `Fn_::expr` and `Fn_::peek`.
2. **The comparison is sound only over program nodes.** A backend types AST it
   builds itself — a lifted lambda's body, a desugared method call — and those
   live in temporaries whose addresses are reused, so two of them collide on one
   `(node, instantiation)` key. A node of the program is alive for the whole
   compile and cannot be aliased. Restricting the gate to recorded nodes removed
   651 phantom classes and is what makes it a gate rather than a rumour.
3. **The monomorphization bound had to move.** `examples/polyrecursion.vyrn`
   reached **18 GiB** before `vyrn-lower`'s worklist took the same bound the
   backends take. `MONO_DEPTH_LIMIT` and `MONO_SIZE_LIMIT` are now in
   `vyrn_frontend::types` beside `type_depth` and `expanded_size`, re-exported
   from `vyrn-codegen`. A bound on monomorphization is not a property of a
   backend, and there are two worklists now.
4. **The checker's answer is not final at the node it is written on.** A generic
   call's arguments are checked against the callee's still-open parameter types,
   so `[]` in `push(xs, [])` is recorded as `Array<T>`. The solution is recorded
   on the call node and applied to the subtree it governs, as an ordered chain
   rather than a merged map, so a caller's `T` and a callee's `T` stay apart. A
   generic RECORD literal does the same thing and needed the same treatment —
   `Deque { front: [], back: ["z"] }` had it, and the lint caught it.
5. **§2.1 item 6 promises a line the AST does not always have.**
   `ast::Expr::line` returns `0` for all five literal forms. A literal inherits
   its statement's line, decided once in the lowering rather than five times in
   whoever prints a position.
6. **The lowering runs the checker again.** `check_and_synthesize` checks, THEN
   synthesizes the JSON codecs, so those functions are never typed by the pass
   that made them. `lower` records over the program it is given, which is the
   only way the codecs get answers at all.
7. **M1's monomorphization is partial, and the residue is counted, not hidden.**
   The worklist follows ordinary generic calls. Higher-order and lambda
   instantiation still lives in the backends, so 9,358 backend answers sit inside
   instances M1 does not build and 1 call could not be followed. Both numbers are
   printed by the gate. M2 is where they go to zero.

### The three deliverables M1 also carried

- **`vyrn emit-lowered`, built and gated here** [A11]. A subcommand beside
  `emit-ir` and `emit-wat`, root module only. Its row in `tests/reproducible.rs`
  landed in the same PR — seven separate processes, bytes compared — and two
  blessed snapshots (`fib`, `option`) landed with it, because an ungated dump is
  the one part of Zig's AIR this design refuses.
- **The lint of §2.6** [A7]. A `debug_assert` inside `lower`, and the same
  function called over every corpus example by the gate. The difference is where
  it is called from, exactly as the amendment said. It found deviations 4 and 6
  above before either reached a reviewer.
- **The two measurements**, written into §2.1 item 1 with their method and the
  threshold they imply.

**M2 — monomorphization moves into the lowering. PARTIALLY IMPLEMENTED, AND IT
FAILS ITS OWN LINE GATE.**

M2 asked for one worklist keyed on type arguments; for `lib.rs`'s two
mutually-feeding worklists and `direct.rs`'s FIFO index queue to become consumers
of it; for `check_instantiations` to stop running `emit()`; and for a net
negative. **Two of those landed, one is a different job from the one written
here, and the line gate is missed and says so** — §3's rule, and RFC-0094's
precedent.

| PR | What | Changed lines | Files |
|---|---|---|---|
| M2a | the lowering's worklist gets module state; both backends' instance LISTS become readable and are gated against it; this text | 636 | 5 |
| M2b | `vyrn check` stops running a whole backend to ask one question; the shell rule that never fired goes | 103 | 7 |

**What landed.**

- **The lowering's worklist is complete for the calls a program writes, and
  there is a gate that says so.** Both backends now record every body they decide
  to emit (`vyrn_codegen::observe::Inst`, off by default like the type sink M1
  added), and the corpus gate asserts set equality against the lowering's list on
  `(callee, type arguments)`, resolved through every declared alias so one
  instance has one spelling. **Zero instantiations are emitted by a backend and
  missing from the lowering.** Seventy-two go the other way, and both classes are
  named the way M1's five type rules are: `GenFn` (72 — a `gen fn` runs in the
  compiler at generation time and is never called in a shipped binary, so neither
  backend emits a body for it) and `ImplicitDispatch` (26 — finding 5 below).
  A third rule was written and then deleted, because it never fired: a backend
  skips the first-order definition of a function with a `fn`-typed parameter and
  emits RFC-0023 specializations instead, and **a specialization keys back to the
  same `(callee, type arguments)` the lowering built**. So the higher-order shell
  is not a keying difference, and a rule that cannot fire is worse than none —
  it invites a reader to believe in a difference that is not there.
- **`vyrn check` no longer runs the native backend** (M2b).
  `check_instantiations` lowered the whole program to LLVM text, matched one
  needle and threw the module away; it reads the lowering's worklist now, and the
  refusal is worded by the same `check_inst_depth` both backends call, from the
  same two constants. That move needs the paragraph above under it: a bound the
  lowering enforces predicts a build only if the lowering reaches every
  instantiation a backend reaches, which is what M2a's gate asserts — with one
  named exception, `ImplicitDispatch`. So the residual risk is exact and it is
  small: a program whose instantiation chain grows without bound *only* through
  an implicitly dispatched `release` or `for`-in step would be refused by a
  backend and not by `check`. Nothing in the corpus is that program, and M4
  closes the class rather than widening the gate.

**What M2 measured, which is not what it went looking for.**

1. **M1's residue was 9,358 answers "inside higher-order/lambda instances M1 did
   not build", and that attribution is wrong. The split is 9,355 to 7.** Only
   seven were about a node the lowering had walked, at an instantiation it had
   not built. Every other one is about **AST that is not in the program**: a
   backend `clone()`s a `Function` before lowering a specialization or lifting a
   lambda, so those bodies' node addresses are the clone's and no worklist can
   make them match. **The residue does not go to zero when the LIST moves. It
   goes to zero when the BODY does.** The gate counts the two separately now and
   asserts the first is zero.
2. **The seven were module state, not higher-order anything.** `std/stream`'s
   `let mut cells: Slots<CursorCell> = newSlots()` is an instantiation both
   backends emit from inside their synthesized initializer, and M1's worklist
   rooted at `program.functions` and never walked a `program.globals`
   initializer. One more root, and the counter is zero.
3. **"1 call the worklist could not follow" cannot go to zero, and asking it to
   was asking for a limit to be deleted.** The single entry is
   `examples/polyrecursion.vyrn`'s `f -> f`, refused by `MONO_DEPTH_LIMIT` —
   the bound working, and the same refusal both backends make. M1 kept one
   counter for three different facts; the reason is a typed value now (`Why`),
   and the gate asserts every entry is the bound instead of counting them.
4. **M1's residue number is not reproducible, and that is a property of the gate
   rather than of the compiler.** The same binary printed 9,569, 9,362 and 9,486
   on three consecutive runs. It counts distinct `(node address, instantiation)`
   keys, and a synthesized node's address is a freed temporary the allocator
   hands out again, so two of them collide — or do not — per run. The halves that
   matter (`compared`, `differed`, and both instance lists) are stable run to run;
   the synthesized count is reported and not asserted.
5. **One class of instantiation a backend has and the lowering does not is
   deliberately still open: `ImplicitDispatch`.** Twenty-six, every one a
   flattened protocol-impl method the source never calls — the `release` a scope
   exit reaches through `impl Owned for Slots<T>`, the `size` a `for x in s`
   reaches through `impl Iterate`. These are calls the *language* writes, placed
   by the release walk and by the loop lowering. Guessing them ahead of the pass
   that PLACES a release would be a second source of truth about where a release
   happens, which is the failure mode `direct.rs:848`'s own worklist comment
   warns about. **M4 puts the release steps in the form; the instantiation then
   comes from the step, and this rule goes with it.** The gate names the class so
   it cannot hide an ordinary miss.

**What did not land, and the measurement that says why.**

The headline sentence — "both worklists become consumers of a list the lowering
hands them" — assumes a worklist entry is an *identity*. It is not. `direct.rs`'s
`Pending` carries an `Rc<Function>`: a substituted clone of the callee with
capture parameters prepended, a lambda's expression body already turned into a
block, a per-target `Sig`, and a wasm function **index reserved at discovery** —
`direct.rs:857` says the order IS the numbering. `lib.rs`'s `HoInst` carries a
resolved `target_sym` per `fn`-typed parameter, and a `target_sym` exists only
after a lambda has been lifted. A backend cannot consume a list of names and type
arguments; it needs the **bodies**. Producing those in the lowering means moving
RFC-0023's specialization and RFC-0037's lambda lifting — capture analysis
included — below both backends, and having each build its own signature from one
shared shell.

That is the same change finding 1 names as the only way the 9,355 go to zero, and
it is a milestone rather than the second half of a PR pair. It is not written as
M2's remainder here because it also decides open question 6.1: a synthesized
shell has to be owned by something, and today it is owned twice.

**The gate, and the verdict.** M2's gate was "net negative. Two worklists and a
whole-backend round trip go." The round trip went; the two worklists did not.
**The pair is 701 changed lines against `main` — 620 added and 81 removed, a net of +539 — so
M2 does not meet its gate and this paragraph is where it says so** (§3's rule;
RFC-0094 M1 is the precedent, and the bar there was moved rather than recorded).
About half of M2a is the instance gate and its rules, which is the cost of the
claim rather than an excuse for it. The deletion M2 promised is priced above and
belongs to the milestone that moves the bodies. **M3's gate does not inherit
this debt**: M3 deletes `peek` and its satellites and is measured on its own
−1,200.

One thing M2b bought that is not a line count. `vyrn check` built a complete LLVM
module for every program it was asked about, and the crate boundary is why: the
front end could not ask a question that only a backend knew the answer to. It
asks the lowering now, and `vyrn-codegen` gained a dependency on `vyrn-lower`
rather than the other way around — the direction §2.1's diagram draws, and the
one M3 needs anyway.

**Emitted output is byte-identical, by construction.** Every hook M2a adds is
behind `observe::on()`, off outside the corpus gate, and nothing in either
emitter's decision path changed. Symbols keep the readable mangle and #165's
structural hash, untouched. Full parity is green at both PRs.

**M2c — the backends consume the body. PARTIALLY IMPLEMENTED, AND THE MILESTONE
ITS BRIEF DESCRIBED IS NOT THE ONE THE CODE NEEDED.**

M2 and M3 both ended at the same sentence: a backend `clone()`s the AST before it
lowers a specialization, so 9,505 backend answers are about nodes the program
does not have and no recorded type can reach them. The remedy both priced was
**moving RFC-0023's specialization and RFC-0037's lambda lifting into the
lowering**, so that a backend consumes a list of bodies rather than a list of
names.

**Measured first, that price is mostly for a clone that bought nothing.** The
9,505 split by engine as 3,483 native, 8,533 wasm and 2,162 `peek` (an answer may
be counted by more than one engine). The native backend was never the problem: it
walks `funcs[name].body` for a generic instantiation, `callee.body` for an
RFC-0023 specialization, and the literal's own `LambdaBody` for a lifted lambda —
the program's nodes in all three. The direct backend deep-copied a `Function`
**twice** for each of the first two: once into `Cx::generics` / `Cx::higher_order`
at startup, and again in `Cx::instantiate` per instantiation. Nothing read the
copy. A specialization differs from its callee in its SIGNATURE — the type
parameters are gone, a `fn`-typed parameter has become the captures its target
needs — and never in its body, and `Cx::sub` already substitutes every type the
body asks about.

| PR | What | Changed lines | Files |
|---|---|---|---|
| M2c | the direct backend borrows the callee's block instead of copying it; the residue gets a ceiling; this text | 328 | 3 |

**What landed.** `Pending` carries the shell and a `body: Option<&'a Block>`
borrowed from the checked program. `Cx` gained the program's lifetime, so
`generics` and `higher_order` hold `&'a Function` and the two `.cloned()` calls at
the call sites became `.copied()`. **9,505 → 4,547**, and about 5,000 backend
answers are compared against a recorded type for the first time. Two classes of
disagreement grew because of it and both were already named — `LessSpecific`
676 → 1,525 and `SameAfterResolve` 356 → 597 — which is the shadow-PR shape: the
answers were always there, and nothing could see them.

**Emitted output is byte-identical, and the index order specifically.** All 161
examples, both backends, `emit-wat` and `emit-ir` hashed against `main`: zero
differences. The wasm function index is handed out by `Module::reserve_func` at
discovery, and discovery order is unchanged because nothing about *when* a body
is enqueued moved — only what the entry points at. The WAT is where a renumbering
would show, and it does not.

**What did not land, and the measurement that says why.** The remaining 4,547 is
AST a backend has nothing to borrow, and it is **not** mostly lambda bodies:
lowering every lifted lambda in the whole corpus records **532** answers. The
residue was classified by expression kind, and the two engines' distributions are
the same shape to within a few per cent:

| Kind | native | wasm | `peek` |
|---|---|---|---|
| `Var` | 1,184 | 1,917 | 412 |
| `Field` | 858 | 934 | 388 |
| `Binary` | 647 | 689 | 1 |
| `Int` | 249 | 428 | 267 |
| `Call` / `@call` | 329 | 547 | 56 |

**Two backends do not accidentally synthesize the same tree, and these are not
backend AST at all — they are `vyrn_frontend`'s own desugars, run at lowering
time.** `project::inline` (`project.rs:224`) clones a `place at` / `place atSet`
body per index site, renames its bindings and substitutes the receiver; the
method-call rewrite builds a receiver `Var`; the interpolation desugar builds a
`Binary` spine. Every one is shared code, called from both emitters, producing a
tree the checker never saw and the lowering never recorded.

So **the −1,200 stays re-parented, and it is now parented onto something §2.1
already promised.** The form is "the checked program with the answers written on
it **and the sugar gone**", and the sugar is not gone — it is expanded twice, once
per emitter, after the lowering has run. `peek` and `static_ty` can read a
recorded type at every node of a generic instance and of an RFC-0023
specialization today; what they cannot read is a desugared node. M3's delete half
therefore needs the desugars to run ONCE, before `lower`, over the program the
lowering records — which is a milestone of its own and is a smaller and better
defined one than "the lowering owns specialization and lambda lifting" was.

Borrowing cannot reach that class, and the reason is worth writing down so nobody
tries: an `Expr` a backend builds during the walk cannot outlive the walk, so
threading the program's lifetime through `Fn_`'s methods stops at the first
desugar site. The choice is to desugar before lowering, or to give the
synthesized nodes an arena. The first is the design; the second is a workaround
for not having done the first.

**The gate, and the verdict.** This milestone's gate was net negative. **It is
328 changed lines across 3 files — 266 added and 62 removed, a net of +204 — so
it does not meet it, and this paragraph is where it says so** (§3's rule;
RFC-0094 M1 is the precedent, and the bar there was moved rather than recorded).
Most of the addition is prose: 117 lines are this text and about 50 more are the
doc comments recording why a body is borrowed and the ceiling that stops it being
copied again. The code is a net deletion — two `Function` deep copies per
instantiation and two more per higher-order call site — and the milestone that
was supposed to carry the −1,200 still has not.

**§6.1 gets its number from the other direction.** The question was owned or
borrowed, and M1 answered "borrow, during the migration" with an owned cost of
1.71×. M2c is the first milestone to spend that answer rather than defer it: the
9,505 → 4,547 was bought by making a backend borrow what it had been owning, at a
memory cost of **zero** — the copies deleted here were pure waste, so the saving
is 1.0× rather than a trade. The 256 MiB threshold is untouched and unreached.

**M2d — the sugar is expanded once. IMPLEMENTED, AND ITS OWN MEASUREMENT SAYS
THE SUGAR IT WAS BRIEFED FOR IS A THIRD OF WHAT WAS LEFT.**

§2.1 says the form is "the checked program with the answers written on it **and
the sugar gone**", and M2c ended by naming the sugar that is not gone:
`project::inline` (`project.rs`), which inlines a `place at` projection AT its
access site. Nobody wrote those nodes. Each engine built its own copy of them,
after `lower` had run, so 4,547 backend answers were about AST no side table
could reach.

| PR | What | Changed lines | Files |
|---|---|---|---|
| M2d-a | the access site asks the LOOKUP, not the expansion; one `store_index` for both backends | 293 | 3 |
| M2d-b | one expansion, shared by the lowering and both backends; the lowering walks it; the ceiling drops; this text | 354 | 5 |

**M2d-a, measured before it was written.** Every engine asked
`project::for_site`, which falls back to the SEEDED row for a builtin
container — `yield @slot(self, i)` — inlined it, compared the result to the
nodes it already had, found the substitution was the identity, threw the copy
away and lowered the originals. Over the corpus that is **20,205
clone-rename-substitute rounds, every one discarded**, against 164 that were
real. The interpreter never did it: it looks the receiver's key up and answers
`None`. `project::site` is that shape, shared; `for_site`, `resolve`, `seeded`
and `Projection::is_identity` went with the question they existed to ask. The
two backends' `store_index` was the same thirty lines twice, refusal wording
included (§1.1's shape), and is one function now.

**M2d-b: what "desugar once" turned out to mean.** The brief said desugar below
the checker and above the emitters, so that every node an emitter walks is a
node the lowering recorded. **The first half of that is not available at the
price it sounds like, and the reason is placement.** A backend emits a
projection's prologue *at the point in the expression walk where the access site
is reached* — in `f() + xs[i]`, after `f()`. Rewriting the program's AST to
hoist that prologue to the enclosing statement moves side effects, which is a
different program, which the byte-identity invariant refuses. So the sugar
cannot be *removed* from the tree without changing evaluation order; what it can
be is **expanded once**. `project::Memo` is a value that shares every expansion
built while it is alive, keyed by `(receiver node address, receiver type key,
method)` and verified on a hit by comparing the receiver and the arguments — a
node address is handed out again after its node dies, and a memo that answers
from a dead key is a miscompile rather than a slow path. The tree is leaked
deliberately: `own`, `movecheck` and this RFC's own rows all key by node
address, so an expansion has to outlive every walk that records against it.

The lowering walks the same expansion, at the site it belongs to, and
`vyrn emit-lowered` renders it — §2.7's dump showing a decision the source does
not contain:

```text
call @at : Int64                        @1
  var w : Window                        @1
  int 0 : Int64                         @60
  call @at                              @20
    field .data                         @20
      var w                             @1
    binary Add                          @20
      field .start                      @20
        var w                           @1
      int 0 : Int64                     @60
```

`for x in c` over a user container is the same thing one level up —
`project::iterate_loop` clones the user's whole loop body into the shape the
projection needs — and is shared the same way.

**The rows carry no type, and that is a statement rather than an omission.** The
checker holds no answer at a node it never saw, and deriving one here would be
the sixth copy of the derivation §1.2 counts five of. So this milestone moves
answers out of the "no row at all" column and into the "row with no type"
column: **`synthesized` 4,605 to 3,294..3,484, `unrecorded` 526 to 4,707.** A
row with no type is a place a type can go; no row at all is not. Typing them
needs the correspondence between an expansion's nodes and the projection body's
— which the checker HAS typed, at `checker.rs:1191` — and that is the next PR,
not this one.

**And here is the measurement that corrects the brief that ordered this work.**
M2c wrote: "these are not backend AST at all — they are `vyrn_frontend`'s own
desugars". Re-measured with the desugars shared, **that sentence is a third
right.** The residue is printed by engine and by expression kind now, and its
shape is:

| class | M2c | after M2d |
|---|---|---|
| `Wasm/var` | 1,917 | 1,458 |
| `Native/var` | 1,184 | 590 |
| `Wasm/field` | 934 | 367 |
| `Wasm/binary` | 689 | 349 |
| `Native/field` | 858 | 313 |

Everything halved and nothing vanished, which is the signature of a *second*
source rather than a remnant of the first. It is the **receiver a backend builds
on the stack to reach an implicitly dispatched call** — the `release` a scope
exit reaches through `impl Owned`, the `size` a `for` reaches through
`impl Iterate`, the `success` a `?` reaches through `Fallible`. `direct.rs`
alone constructs an `Expr::Var` at nine such sites. That is the
`ImplicitDispatch` class §3 M2 already named and already parented onto **M4**,
which puts the release steps in the form; the rest is the lifted lambda, which
§3 M2c measured at 532. So the desugar-once milestone reaches the sugar the
SOURCE writes, and the sugar the LANGUAGE writes is M4's, as M2 said it would
be.

**One measurement that came free, and one comment it falsifies.** `project.rs`
said of the `@b{tag}` / `@p{tag}` names an inline gives its temporaries: "The
names never reach the emitted output (a slot is `%tN`)". True of wasm, false of
LLVM — the native backend mangles them into its alloca names (`@p3.h` becomes
`%_p3_h`). The tag is a process-global counter, so deleting 20,205 inlines
renumbers it, and 12 of the 322 example/backend outputs differ in exactly those
names.

**Byte identity.** All 161 examples, both backends, `emit-ir` and `emit-wat`
hashed against `main`: **322 of 322 identical once `_pN_` / `_bN_` is
normalized, and 310 of 322 identical raw** — the 12 are the rename above, and
normalizing is a mechanical check rather than a judgement. M2d-b changes nothing
at all: its output is byte-identical to M2d-a's, raw, because the `Memo` is
opened by the corpus gate and not by the CLI. A `vyrn build` runs one backend
and no lowering, so it has nothing to share with; the sharing matters where a
program is lowered AND emitted in one process, which is the gate today and every
consumer once M3's delete half lands. Full parity is green at both PRs.

**The gate, and the verdict.** This milestone's gate is §3.0's: net negative
across the engines. **M2d-a is 124 added and 169 removed — a net of −45 — and
M2d-b is 354 changed with 25 removed, so the pair is +284 and does not meet it,
and this paragraph is where it says so** (§3's rule; RFC-0094 M1 is the
precedent). The split is honest rather than excused: the deletion is real and
small (the seeded inline, one duplicated `store_index`, four call-site triples),
and the addition is a mechanism plus the prose that says why it exists — about
90 lines of M2d-b are doc comments recording the address-reuse hazard and the
leak, and 60 more are the gate's new residue report. The deletion this arc was
promised still belongs to M3, and M3's blocker moved rather than cleared: it was
"a backend cannot read a type at a node the program does not have", and it is
now "a backend cannot read a type at a node the form holds but has not typed".

**Three places this milestone's brief did not survive contact with the code.**

1. **Desugaring "before `lower`" and desugaring "into the program" are not the
   same move, and only the first one is available.** The brief and §2.1 both
   read as the second. The prologue's placement is mid-expression; see above.
2. **`a[i] = v` is not shared, and the reason is a type the checker has and
   throws away.** An `IndexSet` names a variable, not an expression, so there is
   no node `checker::record` could have written a type on, and the lowering
   cannot work out the receiver's type at that statement. Both backends work it
   out for themselves and pass it in. 32 sites and 286 nodes over the corpus —
   the smallest of the three — and closing it means the checker recording the
   binding type it already computed at `checker.rs:3251`, which is §1.2's
   complaint word for word and belongs with the milestone that adds the types.
3. **The interpreter consumed the form without being asked to.** §2.4 allows a
   named difference here and the brief allowed one; none was needed.
   `project::site` and `project::iterate_loop` are what the interpreter already
   called, so it shares the same expansion when a `Memo` is open. It keeps one
   copy the backends do not have — `Interp::project_store`, the `a[i] = v` path
   of finding 2, which looks its receiver up by the runtime value's key rather
   than by a static type.

**A hazard this milestone found in the recording, which is not about
projections.** The lowering's first version read `checker::record`'s answer at
expansion nodes, and the corpus gate came back with 192 classes of disagreement
— `Float32` recorded where a backend said `Handle<Person>`. The cause is that
`checker::record` keys by node address and types AST that does NOT outlive the
check: `prelude::all()` builds its seeded rows fresh, a schema builds a
predicate. Those addresses are freed, the allocator hands them out again for an
expansion's own nodes, and a lookup then answers with a dead node's type. The
fix is that an expansion asks for no recorded type at all, but the hazard is
general: **any future consumer that looks a node up in `Recorded` by an address
it did not get from the program can be answered by a corpse.** It is written
here because the next milestone is exactly such a consumer.

**M3 — the backends read types. THE SHADOW HALF IS IMPLEMENTED TWICE OVER; THE
DELETE HALF IS NOT, AND M3 FAILS ITS OWN LINE GATE TWICE. The third measurement
of the blocker is the last one this milestone can make: it is M4's class, whole.**

> **The gate line is CLOSED, by M6's second phase, as permanently unmet.** The
> −1,200 was re-parented four times — onto M4, then M5, then M6 — and each
> re-parenting was written from a bucket. Measured by CLONE rather than by
> expression kind, `peek`'s off-program answers are 68 inside a lifted lambda's
> body, 122 inside a copied `where` predicate and **109 that no sharing move can
> reach**: the release receiver, the calls the emitter builds to reach a
> dispatched method, and their operands — all of them facts about the backend's
> own locals, which §2.3 assigns to the backend on purpose. A `peek` that must
> answer 109 cannot be deleted. What is available is a shrink, with its own
> number; the −1,200 is not it. See §3 M6's second phase. The ledger below stands
> as the record of what this milestone asked for.

M3 asked for `peek` (510), its four satellites, `static_ty`, both `expect`
stacks, both copies of `expected_fn_sig` / `fn_arg_param_types` /
`resolve_fn_arg`, `declared::type_of` and `solve_param`'s backend call sites to
be deleted, for the `(String, Type)` convention in `lib.rs` to become `String`,
and for at least **−1,200 lines** across the two backends. **PR A landed and
nothing was deleted, so the gate is missed by the whole of it** — §3's rule, and
RFC-0094's precedent. Why, and what has to happen first, is below.

**[A10] And the size of that convention change is 16 functions, not ~300.**
`Result<(String, Type), String>` is returned by **16 functions** and the spelling
appears **40 times**, in a file with 400 `fn`s; §1.2 records where the wrong
number came from. Re-measured at M3: the exact return spelling is **33** and
`(String, Type)` anywhere in `lib.rs` is **44**, so the axis is right and the
figure has drifted by a few sites since `dd3a9fe`. So M3 is smaller than advertised on the signature
axis and larger on the other one: `peek`'s **49 call sites**, verified exactly,
are the work.

| PR | What | Changed lines | Files |
|---|---|---|---|
| M3a | [A16]: the form carries the type pair, the gate asserts membership, this text | 444 | 5 |
| M3b | the checker types the expansion, in the caller's scope; the ceiling drops; this text | 151 + this text | 3 + this file |

**What landed: the pair, and the gate's five rules collapsing into it.**

A row carries `ty` — the checker's own answer, the type the value must END UP as
— and `has`, the type it HAS when the node's code has run. `has` is `None`
wherever the two are one answer, which is most nodes. The gate's assertion is no
longer "the backend's answer equals the recorded type" but **"the backend's
answer is one member of the pair"**, and what it measures is:

| Rule | M1 | after [A16] |
|---|---|---|
| *answered the pair's has-type* | — | **21,154** |
| `DefaultedPosition` | 21,148 | 2,114 |
| `ArrayShape` | 3,449 | 1,408 |
| `SameAfterResolve` | 384 | 356 |
| `LessSpecific` | 676 | 676 |
| `Diverges` | 55 | 4 |

Of the **22,321** answers that differed from the destination type, **21,154 are
the has-type** — the other member, which is a different question and not a
disagreement. **1,167 are left.** The rule counts above cover both halves of the
gate, and 3,391 of the 4,558 remaining are the backend-against-backend half,
which a pair cannot arbitrate: two engines disagreeing with each other about one
node is not answered by giving the node two types. The largest class of the
1,167 is `LessSpecific` — the native backend's own substitution keeping a `T`
the recorded answer does not have (`Slots<T>` for `Slots<Int64>`, 143 in one
class) — which §3 M1 already named as the class M3 **deletes** rather than
reconciles, and which is therefore exactly the right residue for a shadow PR to
leave.

**Three things the amendment did not survive contact with.**

1. **The has-type is derived, not read.** [A16] said the form "must carry the
   node's own type"; the checker does not compute one, because it types every
   expression against its destination. So `vyrn-lower` derives it — §2.1 item 2
   records the correction where the item stands.
2. **The largest single class was not the checker's context. It was the
   backends' word size.** 14,564 of `DefaultedPosition`'s 21,148 are
   `Expr::Byte`: the checker answers `UInt8` with or without a destination, and
   both backends answer `Int64` (`lib.rs:5445`, `direct.rs:4790`), because a byte
   literal is an `i64` immediate that narrows at its use. So the pair explains
   these only because **the form's has-type is the backends' `Int64` and not the
   checker's `UInt8`** — a representation answer where the checker gives a type
   answer. That is the right recording, and the reason is the whole point of the
   pair: `coerce(has → needs)` has to be the code the backend actually emits, and
   recording `UInt8` would make the narrowing disappear. It is also a decision
   M5 inherits, because M5 is where `coerce` moves.
3. **`ArrayShape` halved rather than collapsed.** An array literal HAS a
   fixed-size array — `ArrayN(elem, n)` — and ENDS UP whatever it is stored as,
   which is [A16] again one level down. The 1,408 that remain are almost all the
   backend-against-backend half.

`vyrn emit-lowered` prints the pair as `Int64 => Int32` where it has two
members, and the whole corpus of blessed dumps moved **one line**:
`call Err : Result<Int64, Int64> => Result<Age, Int64>` in `option.lowered`,
which is the amendment rendered. Emitted output is byte-identical by
construction: nothing in either emitter changed, and full parity is green.

**What did not land, and the measurement that says why.**

The delete half is blocked on M2's finding 1, and this milestone is where the
brief that ordered it and the code disagree. **9,187 backend answers are about
AST that is not in the program** — a `Function` a backend `clone()`s before it
lowers an RFC-0023 specialization or lifts an RFC-0037 lambda. Those nodes have
no address in the program, so no recorded type exists at them. Deleting `peek`
around them means keeping a second lookup for the bodies the backend
synthesizes, which is two type mechanisms where there was one and is worse than
the duplication it replaces.

So the delete half needs the milestone M2 named: **the lowering owns
specialization and lambda lifting**, and hands each backend a body rather than a
name. That was priced here rather than attempted: `direct.rs`'s `Pending` carries
an `Rc<Function>` plus a per-target `Sig` and **a wasm function index reserved at
discovery — `direct.rs:857` says the order IS the numbering** — and `lib.rs`'s
`HoInst` carries a resolved `target_sym` that exists only after a lambda has been
lifted. 158 lines across the two files mention the mechanism by name. Moving it
is a milestone with its own PR pairs and its own byte-identity risk, not the
second half of this one.

**M2c attempted it and found the price wrong.** Half of the 9,187 needed no move
at all — the direct backend was copying a body it then substituted nothing into —
and `Pending` now borrows the callee's block. The half that is left is
**desugared** AST rather than specialized AST, and the lifted lambda, which this
paragraph named as the second mechanism, is **532 answers over the whole corpus**.
§3 M2c has the classification and the reason borrowing stops where it does.

**M2d then expanded the desugars once and the blocker MOVED rather than
cleared.** The lowering and both backends walk one expansion per access site
now, so the form holds those nodes — but it holds them with no type, because the
checker never saw them and a second derivation here is the thing this RFC
exists to delete. M3's sentence is therefore "a backend cannot read a type at a
node the form holds but has not typed", which is a smaller and better-defined
blocker than the one above and has a named route: the projection's own body IS
checked (`checker.rs:1191`), so the types exist and want carrying across the
inline. §3 M2d has the numbers and the two remaining sources.

### M3b — the checker types the expansion, and the blocker moves a third time

The route above was "carry the projection body's recorded types across the
inline". **It is the wrong route, and measuring why is worth more than the map
it would have needed.** A projection body is checked ONCE, with its impl head's
parameters still open: `place at` on `Slots<T>` yields a `T`, and a backend
cannot use a `T` — carrying that answer onto the expansion would trip the form's
own lint at every generic container in the corpus. The types the expansion needs
are the CALLER's, and only one pass holds the caller's scope.

So the checker types the expansion, where it is inlined, in the scope the access
site is in — `Checker::record_desugar`, ten lines, called at the `@at` site and
at `for x in c`. Two things make it sound, and both were already true:

- **An expansion is leaked** (`project::memo`, `project::iterate_loop`), so its
  addresses are immortal. That is what answers M2d's corpse hazard *for this
  consumer*: a stale `Recorded` entry left by a freed node — `prelude::all()`'s
  rows, a schema's predicate — is OVERWRITTEN the moment the expansion is typed
  at that address, and no later allocation can take it back. The hazard stands
  for any consumer that looks up an address it did not get from a live tree.
- **The diagnostics, the scope and `PENDING_SUBST` all stay inside the call.** An
  expansion cannot fail in a way the source did not already fail; the prologue's
  bindings belong to the access site's block, not to the checker's model of it;
  and the pending substitution is the slot the recording wrapper reads AFTER
  `expr_inner` returns, so an expansion's last generic call would otherwise be
  recorded against the access site's own node.

| counter | M2d | M3b |
|---|---|---|
| `unrecorded` — a row the form holds and has no type for | 4,707 | **78** |
| `synthesized` — an answer about AST the form does not hold | 3,264 | 3,218 |
| `compared` — answers checked against a recorded type | 575,942 | 580,577 |
| `ImplicitDispatch` instantiations | 26 | 24 |

**Three things this milestone's brief did not survive contact with.**

1. **The lexer's rule is not the checker's rule.** `project::iterate_loop` starts
   its cursor at `Expr::Int(-1)`, a node the parser cannot produce — source `-1`
   is a `Neg` over `1` — and the checker refuses a negative literal because "the
   lexer parses literals up to u64::MAX by wrapping into the i64 bit pattern".
   Typing an expansion is the first time that arm ever saw a node the lexer did
   not make. The alternative was to change the loop's shape, which changes the
   emitted bytes; the rule is now conditioned on the one flag that says nobody
   wrote this.
2. **494 of the 4,707 were not desugars at all.** They are the `@panicAt` site
   literal the loader stamps (census U5), deliberately "never checked against
   anything a user could get wrong" — and never typed either, while both
   compiled backends type it. One `self.expr` on the second argument.
3. **The 78 that remain are one class, and it is not a recording gap.** A `Var`
   the checker resolves by NAME rather than by node, because the position must be
   a binding: the receiver of `xs.pop()` and `xs.swapRemove(i)`, and the place
   temporaries `parser::place_receiver` hoists (`s.free[]`). Closing it changes
   what a mutating builtin accepts.

**Byte identity.** All 161 examples, both backends, `emit-ir` and `emit-wat`
hashed against `main`: **321 of 322 identical raw**, and the one difference is
`rest.vyrn`'s generated symbol map embedding the worktree's absolute path — the
same string on both sides but for the directory the checkout is in. No counter
renumbered: `project::inline`'s tag counter advances only where an expansion is
BUILT, and `vyrn emit-ir` runs no lowering and no recording, so the sequence a
build sees is the one it saw before. Full parity is green.

**The gate, and the verdict, and the ledger.** M3's gate was −1,200 lines across
the two backends. **M3b is 151 changed lines and deletes nothing on that list, so
M3 still misses its gate by the whole of it, and this paragraph is where it says
so for the second time** (§3's rule; RFC-0094 M1 is the precedent). Every target,
and why it did not go:

| target | verdict | why |
|---|---|---|
| `peek` (510) + `peek_inner` | **kept** | it answers 52,182 questions from nodes the form holds and **501 from nodes it does not**. A `peek` that must still answer 501 cannot be deleted, and a lookup added beside it is the second type mechanism §1.2 exists to remove. |
| `peek_arm`, `peek_ho`, `gen_peek`, `match_ty`, `join` | **kept** | satellites of `peek`; none is reachable without it. |
| `static_ty` (34), the native `expect` stack | **kept** | same sentence on the native side: 1,678 of its answers are about AST it built during its own walk. |
| the wasm `expect` stack | **kept** | 2,985. |
| both `expected_fn_sig` / `fn_arg_param_types` / `resolve_fn_arg` | **kept** | measured, and they are NOT the duplicate pair §1.1 counts them as: each reads its own backend's structures (`fn_bindings`/`param_types` against `fn_binds`/`cx.sigs`). Merging them is a signature change, not a dedup, and it is not what "read the recorded type" buys. |
| `declared::type_of` | **kept** | it runs in `vyrn-frontend`, for `own` and `movecheck`, before the lowering exists; there is no recorded answer for it to read. |
| `solve_param`'s backend call sites | **kept** | 10 in `direct.rs`. Each one solves the callee's parameters at a call the backend is emitting, and the recorded solution is on the call node — but reading it is the same reader the rows above are waiting for. |
| the `(String, Type)` convention in `lib.rs` | **kept** | re-measured at M3b: the exact return spelling is **33** and `(String, Type)` anywhere in `lib.rs` is **44** ([A10] again). Dropping the `Type` half means the caller reads it from somewhere, which is the row above. |

**And the blocker has a third and final address, which is a milestone that
already exists.** It was "a backend cannot read a type at a node the program does
not have" (M3a), then "at a node the form holds but has not typed" (M2d). It is
now: **a backend cannot read a type at a node it builds DURING its own walk**,
and there are 3,218 of those. The residue names them — `Wasm/var` 1,457,
`Native/var` 589, `Wasm/field` 365 — and every one is the receiver a backend
constructs on the stack to reach an implicitly dispatched `release`, `size` or
`success`, plus the 532 answers inside a lifted lambda's body. **That is M4's
class, named by M2 and unchanged since**: M4 puts the release steps in the form,
so the receiver is a step rather than an `Expr::Var` built at an emit site. There
is no third mechanism left to invent below it, and no further preparation this
milestone can do. **The −1,200 is re-parented onto M4**, and M4 should be read as
carrying both its own −900 and this one.

**M4 — release placement. BOTH PHASES ARE IMPLEMENTED: each one's step 0 found a
live parity defect the corpus had never reached, the placement now covers every
exit kind a body has and one gate holds all three engines to it — 78,371 walks —
and the delete half is a single all-kinds deletion the next phase can now make.
The records are below.** The lowering emits ordered `Release` steps at the
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

**[A9] M4 does not fit one PR pair, so it splits by exit kind.** It is the
largest phase, it holds all three placement defects, and §3.0's budget applies to
it like everything else. The order is the order the bug ledger ranks: **block
exit first, then `break` / `continue` / `return`, then `?` and match-arm
handover.** Each exit kind already has its regression tests in `memory.rs`, so
each split has a gate under it before it starts. This works only if a construct
can be half-migrated — the engines keep their old walk for the exits not yet
moved — which is what open question 1's answer buys (§6.1).

M4 also has a prediction to check first. The interpreter runs no releases on the
`?` path (`interp.rs:2760`) while both backends walk one, and a declared `release`
can print (`interp.rs:2776`). So a program that carries a declaring type across a
`?` should already produce different stderr in the three engines. Either the
corpus never reaches it, or it does and parity is not looking. M4 writes that
program before it writes anything else.

### M4 step 0 — the prediction was right, and the sacred invariant had a hole

**The three engines printed different output, and the corpus had never reached
it.** `examples/releaseacrosstry.vyrn` is the program the paragraph above asked
for: a `Ring` with `impl Owned` whose `release` prints, bound by a `let`, and a
`?` that fails past it. Four shapes — over `Option`, over `Result`, two frames
deep, and an explicit `return` as the control.

| engine | output |
|---|---|
| `vyrn run` | `after option / after result / after nested / release return / after return` |
| native | `release option / after option / release result / after result / release inner / release outer / after nested / release return / after return` |
| wasm | identical to native, byte for byte |

**The two compiled backends agree with each other and the interpreter drops four
lines**, so the fix direction was never in doubt: the interpreter runs what the
backends run. `Expr::Try` propagates as `Ctrl::Return` through the error channel,
and `Interp::block`'s `Err` arm returned without touching the frame's pending
drops, while a `return` statement — `Flow::Return` — has always run them. The
fix is that arm asking which signal it is holding: a `Ctrl::Return` is a function
exit and runs the drops; a `Ctrl::Err` is a trap, and neither backend reclaims
anything on the way out of a trap. **Nine lines of code and a paragraph saying
why.**

Three things this find is worth stating plainly.

1. **It is a defect in the invariant this whole RFC calls the best thing the
   project has built**, and it survived because parity is a corpus and a corpus
   is a sample. §2.4 already said a shared form "does not make three engines
   identical, it makes their differences declared instead of accidental" — this
   was an *undeclared* one, documented nowhere, reachable by a program anybody
   could write.
2. **§1.4's own description of it was too generous.** It records the `?` skip as
   "documented as intentional — the host reclaims", and that reading is right for
   a buffer and wrong for a declared `release`, which is ordinary Vyrn and can
   print, allocate or fail. The distinction the code was making was between
   observable and unobservable reclamation, and it had put a user's own code on
   the wrong side of it.
3. **A shadow milestone found it before its own shadow PR was written**, which is
   the argument for M4 splitting the way [A9] says: the prediction came out of
   reading three walks side by side, and that reading is the milestone.

`examples/releaseacrosstry.vyrn` is in the corpus, so the three engines are
pinned on it now — stdout, stderr and exit code, byte for byte, in the gate that
missed it. Full parity is green with it: 40 tests, 161 examples, no divergence.

### M4 first phase — the block exit. THE PLACEMENT AND ITS GATE LANDED; THE DELETE HALF IS NOT AVAILABLE AT THIS EXIT KIND, AND THE MEASUREMENT SAYS WHY

| PR | What | Changed lines | Files |
|---|---|---|---|
| step 0 | the `?`-path divergence, the interpreter's fix, the corpus pin, this text | 65 | 3 |
| M4a | the form places the block-exit releases; all three engines' sequences become readable and are gated against it; a third blessed dump | 693 + this text | 8 + a snapshot + this file |

M4a is **693 changed lines across 8 compiler files** — 653 added and 40 removed
— plus **149 lines of this text** and a 105-line blessed snapshot, which is 947
against §3.0's ≤ 800 and is said here rather than rounded down. The snapshot is
generated output and this section is prose; the code and its gate are the 693,
and 257 of those are the gate and its fixtures.

**What landed.** `Instance::releases` is a `Vec<Release>` — a block, a binding, a
kind and an exit, in the order they run. It is derived from `own`'s own map and
from source order, in one place, and it is the elaborated form §2.1 item 3 asks
for: `own` answers WHETHER and HOW, and the form answers WHERE and IN WHAT ORDER.
All three engines now report the sequence they walk
(`vyrn_frontend::own::trace`, off by default, in the crate the interpreter can
also reach — `vyrn_codegen::observe` cannot serve three engines), and the corpus
gate asserts it. `vyrn emit-lowered` prints a `release` line per step, and
`ownedcontainer.vyrn` is a third blessed snapshot, because neither `fib` nor
`option` owns a byte of heap and the placement would otherwise have shipped
ungated.

**Measured over the corpus: 52,676 block-exit sequences equal the placement
exactly, and every remaining one is named.**

| rule | count | what it is |
|---|---|---|
| *equal, order included* | 52,676 | — |
| `Terminated` | 1,039 | the block already returned, so no fall-through walk is emitted at all |
| `HostReclaims` | (fixtures) | the interpreter runs 2 of `own`'s 7 kinds; a `String` or an array buffer is the host's |

The interpreter's half is three fixtures rather than the corpus, and the reason
is a shape difference rather than a shortcut: its walk happens when a block
RUNS, so reading it means executing programs, and executing 161 examples
in-process is a different test. Each fixture asks one question — the same
bindings, the same order, and everything skipped a kind the host reclaims — and
the order assertion was checked by breaking it: with `block_exit`'s reverse
removed, the corpus gate reports 40 classes and the fixture prints
`the interpreter released [b, a] where the lowering placed [a, b]`.

**Six places this phase's brief did not survive contact with the code.**

1. **`return` at the end of a body is an EARLY exit in all three engines, so the
   exit kind [A9] ranks first covers the least of a program.** Every function
   body ends in `return`, and every engine treats that as the early path:
   the interpreter's `Flow::Return`, the native backend's `terminated`, the
   direct backend's unreachable tail. `main`'s own block never runs a
   fall-through release walk. That is the whole of the 1,039 `Terminated`, and
   it is why this phase's fixtures put their bindings inside an `if`. The order
   [A9] chose is still the right one — the block exit is the simplest sequence
   and the one the other kinds are defined against — but "block exit first" is
   the smallest of the three, not the largest.
2. **The block-exit portion of the three walks CANNOT be deleted while the other
   exit kinds stay, and that is structural rather than a matter of effort.** The
   per-block frames (`Gen::drop_stack`, `Fn_::releases`, the interpreter's
   per-block `Vec`) exist *for* the early exits: `emit_all_drops`,
   `emit_drops_above`, `emit_loop_exit_cleanup` and `emit_releases_above` all
   walk the same frames from a boundary index. Deleting the fall-through loop
   deletes three lines per engine and leaves the frames, the pushes and the
   boundary indices standing — and replacing it with a lookup into the form
   ADDS a reader beside a derivation that has to stay. **So the −900 M4 is
   priced on is an all-exit-kinds deletion, not a per-exit-kind one**, and the
   split by exit kind buys a gate rather than a ledger entry per phase. The
   deletion this phase banks toward the −2,100 is **zero**, and this paragraph
   is where it says so (§3's rule; RFC-0094 M1 is the precedent).
3. **§1.4's `Rel` sentence is a third right.** It says the direct backend "keeps
   `DropKind` only for two of its seven variants and re-switches on `Type` to
   build a parallel five-variant `Rel` enum", and the re-switch is real —
   `direct.rs`'s `let` site asks `drops.contains_key`, a boolean, and derives the
   kind again. But `Rel` is not parallel to `DropKind`: `Rel::Buffers(Vec<u32>)`
   carries LAYOUT OFFSETS, which §2.3 puts in the backend on purpose, and
   `Rel::Stream`/`Rel::Call` carry the element and receiver types the emit site
   needs. And `rel_for` has two consumers outside the release frames —
   `store_bufs` and `rel_at`'s array arm — so collapsing it is a refactor of the
   store path with its own byte-identity risk, not the deletion §1.4 advertises.
   The measured deletion available there is on the order of 60 lines, against
   `rel_for`'s 48 plus the enum's declaration, and it is not what the −900 is
   made of.
4. **`own`'s `DropKind::Deep` is not always concrete, even in a function with no
   type parameters.** A step was linted for concreteness the way a row's type is,
   and `examples/fnvalarg.vyrn` refused on the first run:
   `let viaFn = defer(label)` in `main` carries `Deep({ run: fn(P) -> T })`,
   because `own` records a declared type's base RECORD SHAPE and the shape keeps
   the declaration's parameters rather than the application's arguments. Both
   backends read the same kind and resolve it the same way, so this is not a
   difference between engines — it is a question `own` leaves half-answered, and
   the walk it produces stops silently at a `Param` field. The lint was dropped
   rather than the finding.
5. **A rule was written and deleted because it could not fire, which is §3 M2's
   precedent applied to itself.** Both compiled backends lower a lifted lambda
   under a shell that owns no release rows (`f_shell`, and `direct.rs`'s comment
   saying so) while `own` records rows inside a lambda like it does anywhere
   else, so a `LiftedBody` rule was written for the difference. **The corpus
   places zero release steps inside a lambda body**, so the difference is real in
   the code and unreachable from the gate. The rule went; the number is here.
6. **The trace had to cross a thread boundary.** The interpreter runs its program
   on a stack of its own (`on_deep_stack`, because a Windows main thread is ~1 MB
   and `CALL_DEPTH_LIMIT` needs more), so a thread-local sink is left behind on
   it. The recording stays per-thread — two tests in one binary run in parallel
   and a global would interleave them — and the thread that made the stack hands
   its rows back.

**`ImplicitDispatch` is still named, and this phase says exactly what closes it.**
It is 24 instantiations: the `release` a scope exit reaches through
`impl Owned for Slots<T>`, the `size` a `for` reaches through `impl Iterate`. §3
M2 parented it here on the reasoning that "the instantiation then comes from the
step" — and the step exists now, so the remaining half is that nothing CONSUMES
it. A worklist entry can only come from a step a backend is reading; while both
backends still derive their own releases, the receiver is still an `Expr::Var`
built at an emit site. It closes with the consumption, not with the placement.

**Byte identity.** Every hook this phase adds is behind `own::trace::on()`, off
outside the gate, and nothing in either emitter's decision path changed. All 161
examples `main` has, both backends, `emit-ir` and `emit-wat` hashed against it:
**322 of 322 identical, raw.** `rest.vyrn` matched too, and the reason is worth
a line because M3b recorded it as the one tolerated artifact: its symbol map
embeds the path the compiler was GIVEN, so a run from inside `examples/` with a
relative filename embeds a relative one and the worktree stops being in the
answer. The artifact is a property of how the emitter is invoked, not of the
emitter. The one extra row on this side is
`examples/releaseacrosstry.vyrn`, which `main` does not have. Full parity is
green, `reproducible.rs` is green, and the blessed dumps moved by one file:
`ownedcontainer.lowered`, added deliberately.

### M4 second phase — the other five exits. THE PLACEMENT AND ITS GATE LANDED FOR EVERY EXIT KIND A BODY HAS, AND ITS OWN STEP 0 FOUND A SECOND UNDECLARED DIVERGENCE — THIS ONE IN THREE CONSTRUCTS AT ONCE

[A9]'s order is "block exit first, then `break` / `continue` / `return`, then `?`
and match-arm handover". This phase is the second and third of those together,
because they are one assertion: an early exit crosses the frames a `match`, an
`if let` and a `for in` open, so a gate that holds `break` and not the scrutinee
is a gate with a hole exactly where #166 lived.

| PR | What | Changed lines | Files |
|---|---|---|---|
| M4b | step 0's fix and its pin; the placement at all six exits; the three engines' walks keyed by the exit node; the gate; a fourth blessed dump | 1,116 | 9 + an example + a snapshot |

**It misses §3.0's budget and this paragraph is where it says so** (§3's rule;
RFC-0094 M1 is the precedent, and phase 1 ran 947). 1,116 changed compiler lines
across 9 files — 858 added and 258 removed — plus a 144-line corpus example and a
186-line generated snapshot, which are not compiler code and are counted apart.
250 of the 1,116 are the gate and its fixtures and about 200 more are doc
comments. **It could not be split further without a stack, and that is a
finding rather than an excuse**: the placement, the three engines' hooks and the
one gate that compares them are a single assertion, and any cut through them
leaves one half that gates nothing and another half that does not compile. The
one clean seam — step 0 alone — would have to be merged BEFORE the rest and then
conflict with it in `interp.rs`, which is the stacked shape delivery forbids.

### M4 second phase, step 0 — the prediction was right again, and this time in three places

Phase 1's step 0 found the `?` path. §1.4's other sentence is that the
interpreter "acts on 2 of 7 `DropKind`s **and 1 of 4 row kinds**", and the second
half had never been written as a program. `own`'s map has four kinds of row: a
`Stmt::Let`, and three temporaries a CONSTRUCT owns — a `match`'s scrutinee, an
`if let`'s (Phase 10a), a `for`-in's iterable (RFC-0092 M5). Both compiled
backends put each of those on a release frame of its own. This engine had a
pending-drop entry for the first and none for the other three.

`examples/releaseacrossexit.vyrn` is the program that asks. Nine shapes, and
three of them printed different output:

| shape | `vyrn run` | native and wasm |
|---|---|---|
| `match` over a temporary | — | `release scrutinee` |
| `if let` over a temporary | — | `release scrutinee` |
| `for` over a temporary array | — | `release elem` |
| `break`, `continue`, `return`, the handover | identical | identical |

**The two compiled backends agree with each other byte for byte and the
interpreter drops three lines**, which is the same verdict phase 1 reached and
for the same reason: a declared `release` is ordinary Vyrn, it can print, and
"the host reclaims" is right for a buffer and wrong for a user's own code.
`Interp::release_temp` is the fix — the construct runs `own`'s row on its own
temporary once it is done with it, on every way out except a trap, which is the
rule `Interp::block` already applied to a `let`. The example is in the corpus, so
all three engines are pinned on it.

Two things this find is worth stating.

1. **It is bigger than phase 1's**, which was one path in one construct. This is
   three constructs, and the shape a `match` arm hands its payload out of is
   exactly the one #166 was — the defect the bug ledger ranks most expensive.
2. **The method transferred exactly.** Both finds came from reading §1.4's own
   prose against the three walks and writing the program the prose predicts.
   Neither needed the milestone's machinery; both were found before the shadow PR
   was written. That is the argument for [A9]'s split, made twice.

### What landed, and what the gate holds now

`vyrn_frontend::own::Exit` is the exit vocabulary — `Block`, `Scrutinee`,
`Break`, `Continue`, `Return`, `Try` — and it lives in `own` rather than in
`vyrn-lower` because all three engines report against it and the interpreter
cannot import that crate. One enum, no translation table between the placement
and the walks it is compared to.

**The keying is the node the exit is AT, and it was chosen for the reader the
deletion phase will be.** `Release::site` is a `Block` for a fall-through, the
`match` / `if let` / `for in` for the temporary a construct owns, and the
`Stmt::Break` / `Continue` / `Return` or the `Expr::Try` for an early one. An
engine standing at any of those has the node in hand, so it can ask for its steps
without re-deriving a boundary index — which is what `LoopCtx::drop_boundary`,
`Fn_::loops`'s third field and `Flow::Break` propagation are three spellings of.
The lowering keeps one frame stack and one loop-boundary stack, which is the one
model of what `Gen::drop_stack`, `Fn_::releases` and the interpreter's per-block
`Vec` each keep privately.

**Measured over the corpus: 52,676 → 78,371 release walks equal the placement
exactly, and every remaining one is named.**

| exit kind | walks held | named |
|---|---|---|
| `Block` | 52,949 | `Terminated` 1,048 |
| `Return` | 20,787 | `StreamCursor` 6 |
| `Scrutinee` | 3,853 | — |
| `Break` | 582 | — |
| `Continue` | 16 | — |
| `Try` | 184 | — |

`StreamCursor` is the one step the form cannot place: the cursor a
`for x in pull()` owns is not a row of `own`'s map at all — no `Stmt::Let` and no
construct carries it, because RFC-0075 M2b closes a stream's producer from the
loop that made it rather than from a reclamation rule — so both compiled backends
walk a frame entry the placement has nothing for. The rule names it rather than
filtering it.

The interpreter's half is nine fixtures rather than the corpus, for phase 1's
reason (its walk happens when a program RUNS), and each one now declares the exit
kind it exists for and asserts that kind was actually reached. A fixture whose
shape stopped producing the walk it was written for would otherwise pass by
comparing nothing.

**Both directions were checked by breaking them**, phase 1's method: reversing
the frame order makes the corpus gate report 30 classes across both compiled
backends and the fixture print
`the interpreter released [a, b] where the lowering placed [b, a]`; replacing the
loop boundary with `0` makes it report
`Native released 1 bindings at one Break exit, the lowering placed 8`.

### Four places this phase's brief did not survive contact with the code

1. **The `0` in phase 1's trace was four things, and three of them had a node all
   along.** Phase 1 recorded a frame entry with no `let` behind it as `0` and
   listed "a match scrutinee temporary, a `for`-in iterable, a stream cursor".
   Three of those are `own` rows keyed by the construct, so they are placeable and
   are placed; only the stream cursor is genuinely nameless. The rule budget the
   brief expected for anonymous frames turned out to be one rule firing six
   times.
2. **One record per frame does not concatenate, and a fixture found it in the
   first run.** The trace was written to emit one record per frame and let the
   reader join consecutive records under one key — which is right for a compiled
   backend, which emits the whole walk at once, and wrong for this engine: a
   release is ordinary Vyrn, so running one emits the CALLEE's own block exits
   into the log between two frames of the walk being recorded. `trace::joining`
   is the fix — a frame reserves its place before it releases anything, and the
   frame outside it appends to the same reservation. The reader is then trivial
   and the model of the walk stays in the engine that has one.
3. **The interpreter's walk has no node to attribute itself to.** `Flow::Break`
   and `Ctrl::Return` carry no AST, so the statement that raises the signal leaves
   the site behind (`own::trace::leaving`) and each unwinding frame reads it. It
   is recording only, off unless a gate asked, and it is the one thing the trace
   needs from this engine that it does not need from the other two. `Expr::Try`
   sets it at each raise rather than once above them, because the `Fallible` path
   calls user code first and a `return` inside that callee would otherwise be the
   last site left behind.
4. **A lambda's frames are not its enclosing body's.** Both compiled backends
   lower a lifted lambda under a shell that owns no release rows, so a `return` or
   a `?` inside one unwinds its own frames. The lowering sets the stacks aside for
   a lambda block body. The corpus still places zero steps inside one — phase 1's
   finding 5, unchanged — so this is a correctness argument rather than a measured
   difference.

### The deletion preconditions, checked off or named

Phase 1 priced the −900 as an all-exit-kinds deletion, so this phase ends with
the checklist the next one reads rather than with a ledger entry.

| precondition | state |
|---|---|
| every exit kind placed | **yes** — `Block`, `Scrutinee`, `Break`, `Continue`, `Return`, `Try`. There is no seventh: the six are every place either compiled backend calls a release walk from. |
| every engine gated at every kind | **yes** for the two compiled ones, over the corpus. **Fixtures** for the interpreter, one per kind, each asserting its kind was reached. |
| the placement reachable without a boundary index | **yes** — `Release::site` is the node the exit is at, and `vyrn-cli/tests/lowered.rs`'s `placement()` is the reader written against it. |
| `ImplicitDispatch` receivers reachable from the placement | **not yet, and the remaining half is exactly what §3 M2 said it was.** The steps exist and carry `own`'s kind substituted for the instance; what does not exist is a CONSUMER. A worklist entry can only come from a step a backend is reading, and while both backends still derive their own releases the receiver is still an `Expr::Var` built at an emit site. It closes with the consumption. |
| the residue a `DropKind::Deep` leaves | **open, and unchanged from phase 1's finding 4.** `own` records a declared type's base record shape, so the shape keeps the DECLARATION's parameters; the walk stops silently at a `Param` field. The lint that caught it stays dropped. Both backends read the same half-answer the same way, so it is not a difference between engines — but a deletion that makes a backend read the step instead of deriving it inherits the question rather than answering it. |

### M4 third phase — one placement, three readers. THE PLACEMENT MOVES BELOW EVERY ENGINE, AND WHERE IT MOVED TO IS THIS PHASE'S ONE DEVIATION FROM THE RFC

| PR | What | Changed lines | Files |
|---|---|---|---|
| M4c | the placement moves from `vyrn-lower` into `vyrn_frontend::own`; the form carries the substituted view; this text | 702 | 2 + this file |

**§2.1 item 3 puts the release steps in the lowered form, and the deletion phase
cannot read them there.** The consumer list is three engines and one of them is
the interpreter, which is in `vyrn-frontend` and cannot import `vyrn-lower` —
the same sentence phase 2 wrote when it put `own::Exit` in `own` rather than in
the form. The placement is not per-instance anyway: `site`, `binding`, `exit`
and the ORDER are properties of a body and of `own`'s own map, and the only
instance-dependent part is the type a `DropKind::Deep` walks, which every engine
already substitutes at its own emit site.

So `own::place_body` computes it, `Ownership::releases` carries it, and
`Instance::releases` is the substituted VIEW — `Release` and `Exit` are both
`own`'s vocabulary now, re-exported by `vyrn-lower` so the form's readers see no
change. `own::placed(steps)` is the one reader all three engines call: it groups
by `(exit, the node the exit is at)`, which is what makes a consumer a lookup
rather than a fourth derivation of a boundary index.

**The gate is the proof the move is exact**: the same corpus, the same three
engines, the same **78,371 walks equal**, `Terminated` 1,048 and `StreamCursor`
6 unchanged, and the two blessed dumps that render a `release` line unmoved.

**One place this phase's brief did not survive contact with the code.** The
lowering's placement walked the DESUGARS — `project::site`'s expansion at an
`@at` access site, and `project::iterate_loop`'s loop — and `own` cannot: it has
no recorded types, so it cannot ask which `place at` a receiver reaches. It does
not need to. An expansion is a clone, so no node in it is a key of `droppable`,
and no step has ever been placed inside one. The placement walks the program's
own nodes, and the counter above is unchanged by dropping the expansions, which
is the measurement rather than the argument.

**The gate, and the verdict.** This phase adds 389 lines to `own` and takes 259
out of `vyrn-lower` against 54 added there — **702 changed lines across 2
compiler files**, a net of +184 that is a MOVE and not a deletion, and it banks
**zero** toward the −900 (§3's rule). The move is not free: `own`'s copy carries
the doc comments that say why the placement lives below three engines rather
than inside one, which is about 130 of the 389.
What it buys is the precondition the ledger below is measured against: a
placement every engine can read.

**So the next phase can delete**, and what it deletes is the whole of it at once:
the three scope-frame stacks, the three boundary indices, `emit_drop`, `rel_at`,
`deep_release`, and the fall-through walks that phase 1 measured at three lines
per engine on their own. The deletion this phase banks toward the −2,100 is
**zero**, and this paragraph is where it says so (§3's rule).

### M4 fourth phase — the deletion. THE TWO COMPILED BACKENDS CONSUME THE PLACEMENT AT EVERY EXIT AND EMIT BYTE-IDENTICAL CODE, AND THE −900 IS MISSED BY 935 BECAUSE THE THING THAT DIES IS SMALLER THAN §1.4 PRICED IT AND HALF OF WHAT §1.4 NAMED IS THE ENCODER §2.3 KEEPS

| PR | What | Changed lines | Files |
|---|---|---|---|
| M4d | both compiled backends read the placed steps; the frame stacks, the boundary indices and the exit-walk trio go; the trace's site axis and the corpus half of the release gate retire; this text | 819 | 5 + this file |

**What landed.** `Gen::drop_stack` and `Fn_::releases` — a stack of scope frames
each — are a flat `HashMap<binding, …>` in each backend, and every exit is
`emit_releases(exit, at)`: look the node's steps up in `own::placed`, map each
binding to the alloca or the wasm place it lives in, emit. `emit_all_drops`,
`emit_drops_above` and `emit_loop_exit_cleanup` were three functions walking
frames from an index; the first and third are now one line of region balancing
each over the one reader, and the second is gone. `emit_releases_above` is gone
the same way. `LoopCtx::drop_boundary` and `Fn_::loops`' third field are gone:
a `break` reads the steps the placement put at that `break`.

**Byte identity.** All 163 examples, both backends, `emit-ir` and `emit-wat`
hashed against `main`: **326 of 326 identical, raw.** That is the claim this
phase had to make and the only one that matters — the placement was proven equal
to the derivations over 78,371 walks, so consumption is a change of READER and
not of decision, and a single differing byte would have meant the proof was
wrong. It caught one real error in this phase and the error is worth the line:
the first version emitted a stream cursor at every exit rather than only at a
function exit, and six stream examples grew four `__vyrn_stream_close` calls
each. Full parity is green (40 tests, `--ignored`), `reproducible.rs`,
`memory.rs`, `limits.rs` and `genwasm.rs` green, blessed dumps unmoved.

### The gate, and the verdict

**M4's gate is at least −900 across the three engines. Measured, the three
engines are +35: native +24 (+173/−149), wasm +21 (+137/−116), interpreter −10
(+2/−12). The gate is missed by 935 and this paragraph is where it says so** (§3's rule; RFC-0094 M1 is the
precedent, and the bar there was moved rather than recorded). Outside the
engines this PR is −109 in the corpus gate, and M4c's move is −205 in
`vyrn-lower` against +389 in `own`, for a two-PR net of +110 in compiler code.
**819 changed compiler lines across 5 files here, against §3.0's ≤ 800 and
≤ 15** — over by 19, plus 166 lines of this text, and said rather than rounded
down. It could not be cut further without a half that gates nothing: the two
backends' consumption, the trace's retirement and the gate's are one assertion,
because a backend that stops reporting and a gate that still asserts against its
report do not compile together.

**The number is not a shortfall in the work. It is a measurement that says
§1.4's price list was reading the wrong half of each file**, and the ledger
below is the evidence. Three findings inside it.

1. **The three largest line counts §1.4 and this milestone's own text name —
   `emit_drop` (180), `rel_at` (239), `deep_release` (149) — are the ENCODER,
   and §2.3 puts the encoder in the backend on purpose.** `emit_drop(slot,
   kind)` loads an aggregate out of an alloca and writes the LLVM that frees its
   buffers; `rel_at` walks a type through wasm layout offsets. Neither decides
   anything. They were counted as placement because they sit in the same
   paragraph as it, and a milestone priced from a paragraph rather than from a
   call graph prices the wrong thing. **568 of the −900 were never available.**
2. **What consumption actually deletes is the frame stack, the boundary index
   and the walk that reads them, and that is about 150 lines per compiled
   backend rather than 450.** It is replaced by a reader of about 40 lines plus
   the doc comment that says what it replaced, so the net per backend is a
   small positive. Phase 1 predicted the shape of this ("deleting the
   fall-through walk deletes three lines per engine") and predicted the total
   would come from doing every exit kind at once; doing every exit kind at once
   is what this phase did, and the total is still small.
3. **The interpreter has none of what M4 deletes.** Its pending-drop list is a
   `Vec` local to `Interp::block`, so there is no scope-frame stack; `Flow`
   propagation IS the unwinding, so there is no boundary index; and there is no
   exit-walk trio, because each frame runs its own list as the signal passes
   through it. §1.4 prices this engine at ~190 lines of which `release_nested`
   (82) is the by-VALUE encoder — the interpreter's `rel_at`. The −10 here is
   the trace's site axis; the rest is a kept-with-reason below.

### The ledger — every target M4 named

| target | verdict |
|---|---|
| the three scope-frame stacks | **two deleted.** `Gen::drop_stack` → `HashMap<usize, DropSlot>`; `Fn_::releases` → `HashMap<usize, RelSlot>`. The interpreter's is a block-local `Vec` and is **kept**: it is not a stack, and the values in it are read out of a scope frame by name, which is this engine's representation of a place. |
| the three break/continue boundary indices | **two deleted** — `LoopCtx::drop_boundary` and `Fn_::loops`' third field are gone. The third, `Flow::Break` propagation's cleanup role, is **kept**: it is not an index, it is the interpreter's unwinding itself. |
| `emit_all_drops` / `emit_drops_above` / `emit_loop_exit_cleanup` | **one deleted, two shrunk to their non-release half.** `emit_drops_above` is gone; the other two are the region-stack balancing over one call to `emit_releases`. Net −30. |
| `emit_releases_above` | **deleted**, replaced by `emit_releases`. Net −5. |
| `emit_drop` (180) | **kept — it is the encoder.** It writes `load`/`call @free`/`call @__vyrn_str_free` and the deep walk's prologue. §2.3 lists representation and the emitted runtime as the backend's. |
| `rel_at` (239), `deep_release` (149) | **kept, same sentence.** Both are a walk of a TYPE through a target layout. `rel_at` reads `Rel::Buffers`' byte offsets, which §1.4's phase-1 correction already ruled backend-side. |
| `rel_for`'s re-derivation of a kind | **kept, and phase 1's correction is why.** It has two consumers outside the release path (`store_bufs`, `rel_at`'s array arm), and the `Rel` it builds carries layout offsets. The registration site still calls it; the ORDER it used to feed is the placement's now. The measured deletion available is ~60 lines behind a store-path refactor with its own byte risk — phase 1 measured it and left it, and this phase matches that judgement rather than beating it. |
| `direct.rs`'s parallel five-variant kind enum (`Rel`) | **kept**, for the row above. |
| the interpreter's equivalents (`run_drops`, `release_nested`) | **kept.** `release_nested` is the by-value encoder. `run_drops` is 30 lines of which the reverse iteration is one; consuming the order there means looking each binding's value up by name across the scope frames being popped, which is what the `frozen` mechanism exists to disambiguate — a rewrite of the unwinding, with real byte risk, for a deletion of one line. **This is the phase's one deliberate non-consumption, and it is a declared difference in §2.4's sense: the engine that still derives its own order is the one the gate still watches.** |
| the eleven "the other engine does it this way" comments | **five gone, one narrowed, five kept and each names a real difference.** Gone: `interp.rs:2089` and `:2776` (the order is `own`'s now, so there is nothing to mirror), `lib.rs:2049` / `direct.rs:1436` / `interp.rs:2718`'s three separate assertions of "innermost frame first, newest binding first" — the invariant is the order the steps are in. Narrowed: `direct.rs:2986`'s "the textual backend reads the same map with the same key" now also reads the same ORDER. Kept, each a target fact: `lib.rs:4441` (a store leaves three kinds alone), `direct.rs:2699` (`store_bufs` is exact on one side and blunt on the other), `direct.rs:1494` (both `expect` stacks), and the two on the emitted trap wordings, which are M5's. |

### `ImplicitDispatch` — the count did not move, and this phase can say why in one sentence

Phase 2 said the class "closes with the consumption, not with the placement",
and the consumption has landed. **It is still 24.** The reasoning was that a
worklist entry could come from a step a backend is reading — and a step carries
`own`'s `DropKind`, which for `DropKind::Release(f)` is a flattened method NAME
and no receiver type. The backend still parks the receiver under a reserved name
(`REL_RECV`, `@rel`) and goes through the ordinary call path, because that path
is what solves a generic `impl Owned for Slots<T>`'s parameters from the
receiver, mangles the symbol and queues the instance. **The missing half is not
a consumer; it is that `own::release_kind` throws the receiver type away.**
Closing it means `DropKind::Release` carrying the type it was decided for, which
is a change to `own`'s vocabulary and touches `vyrn why --memory` and the LSP
hover that print it. It is §2.1 item 5's job — resolved dispatch — and it is
re-parented onto **M5**, with the count and the exact address recorded here so
the next milestone does not have to find it again. The M2 gate's rule stays
until then.

**M5 closed it, and the address was exact.** One field on `DropKind::Release`, one
solve in the lowering, `ImplicitDispatch` 24 → 0, and the rule is gone. See §3 M5's
first phase — including the one correction this paragraph needs: all 24 were
`release`, and none was the `size` a `for` reaches through `impl Iterate`.

### M3's −1,200 — the receivers did not fall, and the count is the same 3,218

M3's blocker was "a backend cannot read a type at a node it builds DURING its own
walk", and it named the release receiver as the bulk of the 3,218. **Consumption
does not remove those receivers**, for the reason above: a `Rel::Call` / a
`DropKind::Release` still reaches its function through a synthesized
`Expr::Var`, because that is how a generic release's symbol gets solved. So
`peek`'s 501 off-program answers are unchanged, `peek` and its family stay, and
**M3's −1,200 is re-parented onto M5 with `ImplicitDispatch`** — they are one
change, and this phase's contribution is to have measured that they are.

**They are not one change, and M5 measured that instead.** Split by variable NAME
rather than by expression kind, the release receiver is **26 of `peek`'s 501** and
324 of the corpus's 3,243. The other nine tenths are ordinary program bindings
inside trees a backend rebuilds. §3 M5's first phase has the table.

### What retired

- **The trace's site axis.** `own::trace::Site` is gone and `note` lost its
  first argument: both compiled backends stopped reporting, because asserting a
  walk that IS the placement against the placement is asserting a value equals
  itself. What is left records one engine — the one that still derives.
- **The corpus half of the release gate** (`vyrn-cli/tests/lowered.rs`, −109):
  `RelRule::Terminated` and `RelRule::StreamCursor` are unreachable from the
  interpreter and went with it; `HostReclaims` and `rel_rule`'s subsequence test
  stay for the fixtures. The `rel_agreed > 1_000` floor went with the counter it
  guarded.
- The interpreter's nine fixtures stay, one per exit kind, each asserting its
  kind was reached.

### Three places this phase's brief did not survive contact with the code

1. **The `StreamCursor` is not just a named difference — it is a step a consumer
   has to splice, and its position is frame structure.** The placement has
   nothing for the cursor a `for x in pull()` owns, and phase 2 recorded that as
   6 named walks. A consumer cannot ignore it: dropping it leaks the producer,
   and appending it changes the order. Both backends keep a `cursors` stack with
   the registration count each was opened at, and a step registered before the
   cursor is a frame outside the loop, so the cursor runs first. **Only a
   function exit reaches one** — a `break` leaves through the loop's own release
   — which is one `match` on the exit kind and is what byte identity caught when
   it was missing. Roughly 20 lines per backend that exist because `own` has no
   row for the cursor.
2. **A flat map answers `slot_owns` / `place_owns` better than the frames did,
   and the doc comment already said so.** Both asked "does this function release
   this slot" by scanning every live frame; `lib.rs:4606` describes that as "the
   `droppable` fact read as a property of the slot rather than of the block,
   which is what RFC-0087 §4 asked for". A map over the whole body is that
   property. Byte identity over the corpus confirms the widening changes nothing:
   a slot from a sibling block that has already exited is not in scope to be
   stored into.
3. **A placed step whose binding a backend never registered is silently skipped,
   and nothing gates it.** `emit_releases` looks each binding up and `continue`s
   on a miss, which is a leak rather than a wrong free — the rule every release
   path in both backends already follows. The corpus has none (byte identity
   would show one as a missing `free`), but the shadow gate that would have
   caught it is the one this phase retired. The honest statement is that the
   guard is byte identity plus `memory.rs`, and that a future engine adding an
   exit kind gets no warning from the form.

**Byte identity.** Every hook this phase adds is behind `own::trace::on()`, off
outside the gate, and nothing in either emitter's decision path changed —
`emit_all_drops` is `emit_drops_above(0)` plus the region pops now, which is the
same walk it always was. All 162 examples `main` has, both backends, `emit-ir`
and `emit-wat` compared against it: **290 identical, 0 different, 34 pairs where
both sides emit nothing** (an expected check failure, a generator needing a
cache). `rest.vyrn` matched, run from `examples/` with a relative filename, for
the reason phase 1 records. Full parity is green, `reproducible.rs` is green, and
the blessed dumps moved by one line in `ownedcontainer.lowered` — the `return`
step at `main`'s exit, which is the placement rendered — plus a fourth file,
`releaseacrossexit.lowered`, added deliberately because
`ownedcontainer.vyrn` reaches a block exit and a `return` and nothing else.

**M5 — traps and the boundary ladder.** One trap table below all three engines,
holding the 20 wordings and their conditions — in `vyrn-frontend` rather than in
`vyrn-lower`, which is the record below and the answer to §6.4. `coerce`'s ladder is
decided in the lowering; the three engines keep only their leaves. The interpreter
stops re-spelling `IO_MESSAGES`. Gate: the count of trap literals outside the table
is **zero**, and a test asserts it — the same shape as the reserved-name gate
RFC-0094 M2 landed.

### M5 first phase — the receiver type. `ImplicitDispatch` IS ZERO AND ITS RULE HAS RETIRED; AND THE MEASUREMENT THAT COMES WITH IT SAYS M3's −1,200 WAS RE-PARENTED ONTO A CLASS TEN TIMES SMALLER THAN THE PROSE SAID

| PR | What | Changed lines | Files |
|---|---|---|---|
| M5a | `DropKind::Release` carries the receiver type; the lowering solves the instance from the step; `solve_param` moves below both backends; the `ImplicitDispatch` rule retires; this text | 524 | 8 + this file |

**The diagnosis M4d wrote was exact and the fix is one field.** `own::release_kind`
answered `Release(name)` — a flattened method name and no receiver — so a placed
step said WHICH method and not WHICH INSTANCE, and a generic
`impl<T> Owned for Slots<T>` has one body per element type. Both backends already
carried the type beside the name at their emit sites (`Rel::Call(f, ty)`,
`Gen::call_release(v, ty)`); nothing above a backend did. `DropKind::Release` is
`(String, Type)` now — the type `release_kind` was ASKED about, unresolved, which
is exactly what both emitters already pass — and `vyrn_lower::dispatched` solves
the callee's parameters from it and puts the instance on the worklist beside the
calls the source writes.

**Measured: `ImplicitDispatch` 24 → 0, and every one of the 24 was the same
shape.** Listed before the fix, they are `Owned__Slots__release<…>` in twelve
corpus programs, two per program because two backends emit it — `<Int64>`,
`<String>`, `<{ name: String, age: Int64 }>`, the generational-index record, the
linked-list node. **Not one was the `size` a `for x in s` reaches through
`impl Iterate`**, which §3 M2 and M4's own ledger both named beside the release as
if the class were mixed. It never was: the corpus's `for`-in dispatch is either
concrete or already on the worklist, and the class was `release`, whole. The
lowering's instance count is 5,359 → 5,371, `missing` stays 0, and the rule is
deleted rather than left to fire zero times (§3 M2's precedent, applied to itself a
second time).

**One thing this cannot reach, said here rather than left to be found.** A generic
declared release reached only from INSIDE a `DropKind::Deep` walk — an
`Array<Slots<Int64>>` — is not a step; it is a call the ENCODER makes while walking
a type through a target layout, and §2.3 keeps that walk in the backend. The corpus
has none. One would now fail the gate as a missing instantiation rather than hide
under a rule, which is the failure this file prefers, and it is written into
`vyrn_lower::dispatched`'s own doc comment.

### The measurement that re-prices M3, and it is not the one M4 handed over

M4d re-parented M3's −1,200 here on the sentence that the 3,218 off-program answers
are "every one the receiver a backend constructs on the stack to reach an
implicitly dispatched `release`, `size` or `success`, plus the 532 answers inside a
lifted lambda's body". **That sentence is a tenth right, and the way to find out was
to print the variable's NAME.** The gate classifies the residue by engine and
expression kind; `var` was one bucket. Split by name, over the corpus, before this
phase:

| residue bucket | count | what it is |
|---|---|---|
| `Wasm/var[@rel]` + `Peek/var[@rel]` | **324** | the release receiver — the class M4d named |
| `Wasm/field` + `Native/field` | 676 | — |
| `Wasm/binary` + `Native/binary` | 652 | — |
| `Wasm/var[value]`, `[self]`, `[x]`, `[cur]`, `[s]`, `[nums]`, `[cells]`, `[f]`, `[v]`, `[n]`, `[None]` … | ~700 | ordinary program bindings, in trees a backend REBUILT |
| `var[@a0]`, `var[@c0]`, `var[json$arg]` | 132 | argument and capture temporaries |
| `int`, `str`, `call`, `consume`, `record` | ~1,100 | — |

**3,243 total, and `@rel` is 324 of it.** So the release receiver is **10%** of what
M3's delete half is blocked on, not the bulk, and removing it would not have moved
`peek` toward deletable: `peek`'s own off-program answers are 501, of which `@rel`
is **26**. The other 475 are `Peek/int` 193, `Peek/field` 102, `Peek/call` 50 and
named program variables — nodes inside trees the backends rebuild for reasons that
have nothing to do with dispatch (RFC-0023 specialization shells, lifted lambda
bodies, the method-call and interpolation desugars `direct.rs` builds at eleven
further `Expr::Var` sites).

**So this phase deliberately did NOT make the backends stop building the `@rel`
receiver**, and the reason is a price against a benefit rather than an unsolved
problem. The benefit is 324 rows and no deletion. The price is a second entry point
into `gen_call` / `Fn_::call` — 500-odd lines apiece whose whole interface is
`&[Expr]` — taken through the two functions with the highest byte-identity risk in
the compiler, for a class that closes nothing. The receiver type is on the step now,
so a future phase that has a reason can do it in an afternoon; there is no reason
today.

**M3's −1,200 is therefore re-parented a fourth time, and this is the first
milestone to say where it actually lives.** It is not dispatch. It is that both
compiled backends synthesize AST during their walk at more than a dozen sites, and
a recorded type cannot reach a node that does not exist until the emitter builds
it. M2d closed the projection desugars by SHARING the expansion; the remaining
sites are the same shape and the same remedy — share the tree, do not re-derive the
type — and that is a milestone of its own with the residue table above as its
ledger. **The −1,200 is not M5's**, and pretending otherwise for a fourth time
would be the gate-moving §3 forbids.

**Byte identity.** All 163 examples, both backends, `emit-ir` and `emit-wat` hashed
against `main` from `examples/` with a relative filename: **326 of 326 identical,
raw** (34 of the 326 are pairs where both sides emit nothing — an expected check
failure, a generator needing a cache — and they are identical too). Nothing in
either emitter's decision path changed: the second member of `Release` is read by
the lowering and by nobody else, `solve_param` moved verbatim, and
`vyrn emit-lowered` still renders a step as `Release {name}` so the blessed dumps
are unmoved. Full parity green (40 tests, `--ignored`), workspace green (67 suites),
`vyrn-lsp` green, `cargo fmt --check` clean.

**The gate, and the verdict.** M5's stated gates are `ImplicitDispatch` → 0 and the
trap-literal count → 0. **The first is met.** This phase is 524 changed lines across
8 files — 290 added and 234 removed, a net of +56 — against §3.0's ≤ 800 / ≤ 15, and
about 180 of both halves are `solve_param` moving from `vyrn-codegen` into
`vyrn_frontend::types` so the lowering can use the one matcher rather than write a
second (§1.1's shape, avoided rather than added to).

### M5, the trap table — THE COUNT OF TRAP LITERALS OUTSIDE THE TABLE IS ZERO AND A TEST ASSERTS IT; THE TABLE IS IN `vyrn-frontend` RATHER THAN `vyrn-lower`, WHICH ANSWERS §6.4; AND THE GATE FOUND FIVE COPIES §1.3'S CENSUS HAD MISSED

| PR | What | Changed lines | Files |
|---|---|---|---|
| M5b | `vyrn_frontend::trap`; all three engines and the C shim read it; `vyrn-cli/tests/traps.rs` asserts nobody re-spells it; this text | 915 | 15 + this file |

**Where it went, and §6.4 is answered by the placement rather than by an
argument.** Open question 6.4 asked whether `vyrn-lower` should be a module of
`vyrn-frontend` instead of a crate, and said "M5 is when this has to be settled:
the trap table is the first thing the interpreter must import, and
`vyrn-frontend` cannot depend on `vyrn-lower` while `vyrn-lower` depends on the
checker." **The table does not have to be in `vyrn-lower` for that sentence to
bite.** It has to be in the crate all three engines can read, which is
`vyrn-frontend` — the same seam M4c used when it moved the release placement into
`own` for the same reason, and stated in the same words. So `vyrn-lower` stays a
crate, 6.4 keeps its answer from M1 (a crate boundary is what stops the lowering
reaching a backend by accident), and the question is closed rather than deferred
again.

**What moved.** `vyrn_frontend::trap` holds every wording a running program can
die with, in three shapes because there are three: fixed (`DIV_ZERO` and seven
neighbours); split around a runtime value (`ARRAY_INDEX`, and every `IO` entry,
whose `%s` is a path) — the pair is the primitive because the direct wasm backend
has no `snprintf` and concatenates; and filled by a compile-time constant
(`call_depth`, `region_depth`). `line(msg)` is the framing a compiled runtime adds
and the CLI adds for the interpreter, so an engine chooses HOW to say a trap and
never WHAT. `IO_MESSAGES` / `io_message` / `io_message_parts` are re-exports from
`vyrn-codegen` now, under their old names, so no backend call site moved.

**Two deletions came free, and both were a second copy of a message.**

- **The native prelude hand-counted its array lengths.** `@.trap.div0` was
  `[25 x i8]` written beside `"error: division by zero\0A\00"` written beside the
  same sentence in `direct.rs` and in `interp.rs`. `llvm_str` already measures
  what it escapes, so `trap_global(name, msg)` writes both and the count is
  derived. Five globals stop carrying a number a reviewer had to check by hand.
- **The C shim's `out of memory` was a fourth spelling at three sites**, and it
  could not be otherwise: C cannot import a Rust constant. `RUNTIME_SHIM` is
  `RUNTIME_SHIM_TEMPLATE` with a `$OOM` hole and `runtime_shim()` fills it. That
  also fixes something nobody had noticed: the shim's object cache keys on the
  shim's own bytes, so a reworded trap now invalidates the cached object instead
  of being silently linked out of it.

**The gate, and what it found.** `vyrn-cli/tests/traps.rs` reads its needles OUT
of the table — a wording added to `trap.rs` tomorrow is gated the day it lands,
and a list in the test would have been a twenty-first copy — and scans every
`.rs` file under a `src/` of the workspace, `vyrn-lsp`, `vyrn-genwasm` and
`vyrn-play` included. **It is zero, and it was not zero when it was first run:
it named nine sites, and five of them were real.**

| what it found | verdict |
|---|---|
| `vyrn-cli/src/remote.rs:42`, `vyrn-lsp/src/main.rs:229` | two more copies of `` cannot list `%s` ``, in two resolvers §1.3's census never looked at. Fixed. |
| `vyrn-frontend/src/interp.rs:1940` | `` moduleInterface cannot read `{path}`: {e} `` — a generator diagnostic that had re-spelled the I/O payload inside itself. It composes now (`format!("moduleInterface {}: {e}", io_at("readerr", …))`), byte for byte the same string. |
| `vyrn-frontend/src/codec.rs:417` and `loader.rs:45,111` | a third `validation_message` and two more `cannot list`, found by the census that preceded the gate. Fixed. |
| `checker.rs:3312`, `interp.rs:3594` etc. — `array index must be an Int64` | **not a trap**, and the needle was wrong rather than the code. A split wording needs BOTH halves before it is that wording; a prefix alone opens a compile-time diagnostic about a program. |
| two `ir.contains("error: …")` in `lib.rs` | test assertions the scanner had failed to skip, because it counted braces and `lib.rs` holds LLVM function bodies as string constants. It ends a `#[cfg(test)]` module at the next bare `}` in column zero instead — the one thing that survives this workspace's files. |

**Test literals stay, deliberately.** A test asserting
`run(src).unwrap_err() == "array index 5 out of bounds"` is the independent check
ON the table; reading the wording out of `trap` there would assert that a value
equals itself, which is the mistake M4d's own retirement notes name. Comments stay
too — a comment quoting a wording is documentation of the contract, and forbidding
it would delete the sentences that explain the table. The gate was verified by
breaking it: one planted `format!("array index {} out of bounds", 1)` in
`direct.rs` fails the run and names the line.

**Byte identity.** All 163 examples, both backends, `emit-ir` and `emit-wat`
against `main`: **326 of 326 identical, raw.** That is the tightest check this
phase could have: the LLVM prelude's trap globals are IN the emitted text, escapes
and array lengths included, so a single wrong byte in the escaping rewrite would
show. Full parity green (40 tests, `--ignored`) — which is the other half, because
parity compares stderr and stderr is what this phase must not move. Workspace
green (68 suites), `vyrn-lsp` green, `vyrn-play` checks, `cargo fmt --check` clean.

**The gate, and the verdict.** M5's trap gate — the count of trap literals
outside the table is zero, test-asserted — **is met**. **This phase is over
§3.0's budget and this paragraph is where it says so** (§3's rule; RFC-0094 M1 is
the precedent): this phase is 425 changed lines across 13 existing files plus two
new ones, `trap.rs` (260) and `traps.rs` (230), for **915 across 15 files** against
≤ 800 / ≤ 15. It has no clean seam: a table nothing reads gates nothing, and a gate
with no table does not compile, so cutting it would need a stack, which delivery
forbids. About 300 of the 915 is doc comment and this text.

### What M5 did not do, and the measurement that prices it

**`coerce` did not move, and the reason is a size rather than a blocker.** M5 asks
for the boundary ladder's decisions to be decided in the lowering with the three
engines keeping their leaves — §1.5's 505 lines across `interp.rs:6226` (161),
`lib.rs:2364` (146) and `direct.rs:3784` (198). Its input already exists: M3's
`(has, ty)` pair on every row IS the question `coerce(has → needs)` answers, and §3
M3's finding 2 already records that the pair's has-type is "a decision M5
inherits". What is missing is not information; it is a phase. `coerce` runs at
every value boundary in every program, so a plan-in-the-form is a change to the
one function whose output every single example depends on, and it belongs beside
the trap table the way M4's four phases belonged beside each other rather than
inside one PR. **It is not started, it is not blocked, and this paragraph is the
gate saying so rather than the bar moving.**

**M6 — the interpreter runs the lowered form.** For everything M1–M5 moved, the
three engines stop holding three answers, and every place the interpreter still
does something else becomes a **declared** difference with a name — the `?` path
of §1.4 is the first one on that list. Not "parity becomes structural": §2.4 says
why that claim was too large. Until this lands, the interpreter is still a third
copy of what the form decides, and parity is still doing the work it does today.
M6 splits by construct like M4, for the same reason: 2,622 lines of walk
(`interp::expr` 1,847, `interp::stmt` 775) is not one PR, and the fallback to the
old walk for unmigrated arms is what makes an intermediate state legal.

**Order, and why.** Types are first because nothing else can move without them: a
release needs a type to know deep from shallow, and a monomorphization is a type
substitution. Release placement is second-to-last among the semantic moves because
it is where the bugs are and it wants the most gate coverage under it. Traps are
late because they are cheap and independent — the one milestone that could be
pulled forward if M3 turns out to be bigger than measured.

### M6 first phase — the WRITING half of the projection. THE `place atSet` EXPANSION IS ONE SHARED, CHECKER-TYPED TREE AND THE OFF-PROGRAM RESIDUE FALLS 3,266 → 1,955; AND THE SITES M5 HANDED OVER AS THIS MILESTONE'S LEDGER ARE NOT TYPE DERIVATIONS AT ALL, WHICH IS WHY MEASURING THEM FIRST WAS THE WHOLE OF STEP 1

| PR | What | Changed lines | Files |
|---|---|---|---|
| M6a | `place atSet` shares one expansion and the checker types it; the lowering walks it; the residue gate reports by NAME; §2.4's list; this text | 169 code + this text | 6 + this file |

#### The ledger M5 handed over, read one site at a time

M5's residue table said the `−1,200` lives where "both compiled backends
synthesize AST during their walk at more than a dozen sites", and named
"eleven further `Expr::Var` sites" in `direct.rs` as the address. **Read one by
one they are ten, not eleven, and every one of them is the same shape — which is
a different shape from the one the sentence implies.**

| site | the name it spells | what it is |
|---|---|---|
| `direct.rs:1983` | `@a{i}` | the defunctionalized dispatcher's own parameters (RFC-0037) |
| `:2048` | `@c{i}` | the same dispatcher's captures, copied out of the heap block |
| `:2288` | `@rel` | the release receiver, parked so `Rel::Call` reaches `call` |
| `:4414` | `@lazy` | a `lazy T` field being forced through `fnval_call` |
| `:7052` | the receiver's own name | a call through a stored `fn` value |
| `:7661` | `bnd.cap_srcs` | the capture sources of a `fn`-typed parameter, at `target_call` |
| `:7686` | `bnd.cap_srcs` | the same, at `resolve_fn_arg` |
| `:8040` | `cap_names` | the same, at `lift_lambda`'s return |
| `:8253` | `bnd.cap_srcs` | the same, at `fnval_binding` |
| `:11003` | `@try` | the `?` receiver, parked so `Fallible`'s methods reach `call` |

**Every one is a ONE-NODE tree whose type the backend wrote down one line
earlier.** The pattern is identical at all ten: `scope.push((name, place, ty))`
and then an `Expr::Var` naming it, because `call` / `emit_call` / `fnval_call`
take `&[Expr]` and passing a raw address instead would be a second
argument-passing convention beside the one that exists. `peek`'s arm for such a
node is `self.lookup(name, line)?.1` — **the backend's own locals table**, which
§2.3 assigns to the backend in as many words ("Locals. SSA temps and `alloca` on
one side; wasm locals and the shadow stack on the other").

So the answer to step 1's question is the third option and not the first two:
these sites are an encoder concern, they keep their leaf, and no recorded type
could reach them because there is nothing recorded to reach — the type is a fact
about a wasm local, not about a program. **They are ledgered closed by §2.3, and
the `−1,200` does not live here.** What the sentence in M5's record got right is
the class; what it got wrong is which members of it matter, and the way to find
out was to read all ten rather than to price the bucket.

#### Where it did live, and it is one key

The residue's other half is named program bindings — `value`, `cells`, `nums`,
`strs`, `self` — inside trees that carry expressions the SOURCE wrote. M2d closed
one such class by sharing the expansion of a `place at`. **Its writing half was
never shared, and the reason is one line.**

`a[i] = v` expands a `place atSet` exactly as `a[i]` expands a `place at`, but
`Stmt::IndexSet` names its receiver rather than holding a node for it. So
`project::store_index` synthesized an `Expr::Var` on the stack and handed it to
`site`, whose memo is keyed by the receiver's ADDRESS — and a stack temporary's
address is not an identity. The memo therefore missed on every call, each engine
built its own tree of the projection's body AND the move-out/mutate/move-back
group around the stored value, and the checker never saw any of them.

**Keyed on the INDEX node instead** — a node of the program, alive for the whole
compile, verified against the whole site (`name`, `index`, `value`) the way
`Expansion` verifies its own — it is one tree. `Checker::stmt` types it at the
store site with `record_desugar`, which is M3b's move applied to the half M3b
did not reach; `vyrn_lower` reads it by anchor through `project::stored`, because
the lowering stands at a `Stmt::IndexSet` with a NAME and has no scope of binding
types to find the `place atSet` with; and both backends walk the shared block
instead of a `Block` they built.

#### Measured

| | before | after |
|---|---|---|
| NODES no instantiation of the program holds (`Tally::synthesized`) | 3,266 | **1,955** |
| the ANSWERS about them, by engine — `Peek` | 501 | **299** |
| — `Native` | 1,498 | **798** |
| — `Wasm` | 2,834 | **2,152** |
| rows the form holds with no type (`Tally::unrecorded`) | 78 | **78** |
| rows | 353,321 | 354,154 |

The last two are the ones that had to hold. **78 → 78** is the sentence "the
checker types it": a shared expansion the checker did NOT type would have moved
those 833 new rows into `Tally::unrecorded` instead, which is sharing without
deciding and is worse than not sharing. And `answered_has` (21,345), `differed`
(1,327), the cross-backend count (4,503) and every rule tally are unchanged to
the answer, which is what "this deletes nothing on purpose" looks like when the
thing that moved is an identity rather than a decision.

#### `peek`'s off-program answers are 299, and M3's `−1,200` is NOT re-parented a fifth time

**The gate is not met and this is the terminal reason rather than a fifth
schedule.** `peek` can read a recorded type only at a node the lowering
recorded. After this phase 299 answers are at nodes it cannot, and they are two
classes and no more:

- **26 are `@rel`**, and the rest of the reserved-name class shows up under the
  two emitting sites rather than under `peek` (`Wasm/var[@a0]` 74,
  `Wasm/var[@c0]` 25, `Wasm/var[@a1]` 19). They are §2.3's, permanently. A `peek`
  that read the form would still need this arm, so the class does not shrink; it
  is simply not `peek`'s to delete. It is `lookup`'s.
- **The rest are downstream of the one clone this arc has not shared.**
  `Fn_::lift_lambda` clones a lambda's body (`direct.rs:7986`: `b.clone()`, and
  `(**e).clone()` for the expression form), and a clone has addresses of its own —
  so every node inside a lifted lambda is phantom by construction, and so is
  every expansion built while walking one. The remaining buckets are consistent
  with that and with nothing else this phase can point at:
  `Peek/int` 168, `Peek/call` 50, `Native/var[value]` and `Wasm/var[value]` 132
  each. **`value` is worth naming separately**, because it is not a lambda: it is
  the binding a scalar `where` predicate uses, and a predicate lives on a
  `TypeDecl` rather than in any function body, so no instance of the program
  holds it. That one is a third class and a small one.

**So the `−1,200` has an address now, and it is a body rather than a class of
node.** Sharing a lifted lambda's body is the move this phase made, one level up:
leak it, key it by the `Expr::Lambda` node, let the checker type it in the
caller's scope. It is a phase of its own, with a hazard neither M2d nor this
phase had — a lambda body is lowered under a SUBSTITUTION per specialization, so
one shared tree needs the row-per-instantiation the form already carries and the
`@pN` renaming does not. **What is still owed before that phase starts is one
measurement this one did not make: which of the 299 are inside a lifted body and
which are not.** The instrument now exists (the gate reports by name); the
attribution does not, and writing the brief from the bucket instead is the
mistake this section's own ledger catches M5 making.

#### `coerce` did not move, and this time the reason is a signature rather than a size

M5 recorded the ladder as "not started, not blocked, a phase". This phase did not
start it either, and the reason is not the budget: reading the three functions
BEFORE writing the shadow found something §1.5 did not have —
**`Interp::coerce` is a different function.**

| engine | signature | dispatches on |
|---|---|---|
| interp (`interp.rs:6411`) | `coerce(&self, v: Val, ty: &Type)` | the TARGET type and the runtime value's shape |
| native (`lib.rs:2569`) | `coerce(&mut self, op: String, from: &Type, to: &Type)` | the type PAIR |
| wasm (`direct.rs:3902`) | `coerce(&mut self, .., from: &Type, to: &Type, ..)` | the type PAIR |

§1.5 counted 161 + 146 + 198 lines and called it "505 lines of the same
decision". It is 344 lines of one decision and 161 lines of a different one. The
interpreter has no `from` at a boundary because it walks an untyped `Val` — the
static type of what it is holding is exactly the thing this RFC is moving into
the form, so **the ladder can be shared by two engines now and by the third only
once it runs the form**, which is this milestone and not a prerequisite of it.
That is not a reason to defer the move; it is a correction to what the move buys,
and the shadow that lands it should gate two engines and say so rather than gate
three and discover this in its own step 0.

#### Byte identity

All 163 examples, both backends, `emit-ir` and `emit-wat` hashed against `main`
at `46a6a43`, run from `examples/` with a relative filename: **326 of 326
identical, raw** (34 of them pairs where both sides emit nothing — an expected
check failure, a generator needing a cache — and those are identical too). This
is the check that mattered most, because sharing an expansion changes WHICH nodes
an emitter walks: the `@pN.x` temporaries `project::inline` mints now get one tag
per site instead of one per engine, and the reason that is safe is written where
it always was — "the names never reach the emitted output (a slot is `%tN`)", now
verified rather than asserted.

#### Three places this phase's brief did not survive contact with the code

1. **"The gate prints it" — it did not.** The brief said to re-derive the
   eleven-site list from the corpus gate's residue-by-name output. The gate prints
   the residue by expression KIND; M5 got the names by hand-editing
   `observe::kind_of` and the edit did not ship, so the next milestone's ledger
   was a patch rather than a test run. It ships this time: `kind_of` interns
   `var[name]`, the gate prints per-engine totals and the top rows with an example
   to open, and the pool is bounded by the distinct variable names in one corpus.
2. **The count was ten, not eleven, and the shape was not the one the count
   implied.** Recorded above. A bucket priced by size said "trees the backends
   rebuild"; ten reads said "the backend's locals table". Both sentences are about
   `Expr::Var`, and only one of them names something a milestone can close.
3. **The desugar-once sharing is ON only where the gate opens it, and this
   phase inherits that rather than fixing it.** `project::Memo::open()` has
   exactly one caller in the whole workspace and it is
   `vyrn-cli/tests/lowered.rs`. A `vyrn build`, a `vyrn check` and
   `vyrn emit-lowered` open none, so in production every engine still expands for
   itself — including the lowering, whose `desugar` then records against addresses
   the checker's own expansion does not have. The numbers M2d, M3b and this phase
   report are therefore all measured with the sharing on, and they are honest
   about what they measure; what is not yet true is that a released compiler
   behaves that way. The remedy is one `Memo` held across the driver's
   check-lower-emit sequence rather than around the gate, and it is not free:
   the two compiled backends would then walk each other's nodes for the first
   time, and the blessed `emit-lowered` dumps would gain the types the expansion
   rows do not have today. **That is a phase, it is named here, and no milestone
   above should be read as having shipped it.**

#### The gate, and the verdict

**169 changed lines across 6 files** — 147 added and 22 removed — against §3.0's
≤ 800 / ≤ 15, plus this text. The corpus gate's `synthesized` ceiling moves
3,500 → 2,100 and its comment says which sharing each step of 9,505 → 4,547 →
3,484 → 1,955 bought. Full parity green, workspace green, `vyrn-lsp` green,
`cargo fmt --check` clean on all three manifests. M3's `−1,200` is **not met**,
its address is `lift_lambda`, and M5's coerce phase is **not started** for the
reason above.

### M6 second phase — THE SHARING IS ON IN A RELEASED COMPILER, AND TURNING IT ON WAS A DEFECT FIX RATHER THAN AN OPTIMISATION: `vyrn emit-lowered` HAS BEEN PRINTING A DEAD NODE'S TYPE SINCE M2d. THE ATTRIBUTION M6a OWED SAYS `lift_lambda`'s CLONE IS 68 OF `peek`'s 299 — AND FINDS A SECOND CLONE, TWICE THE SIZE, THAT THIS FILE HAD NEVER NAMED. M3's GATE IS CLOSED AS PERMANENTLY UNMET, WITH THE NUMBER

| PR | What | Changed lines | Files |
|---|---|---|---|
| M6b | the driver opens one `Memo` per program; the residue gate reports which CLONE an off-program answer was given inside; a fifth blessed dump; M3's gate closes; this text | 184 | 5 + a snapshot + this file |

#### Piece 1 — production sharing, and what it found

M6a's third deviation said `project::Memo::open()` had exactly one caller and it
was the corpus gate, so "in production every engine still expands for itself —
including the lowering, whose `desugar` then records against addresses the
checker's own expansion does not have". The remedy it named was one `Memo` held
across the driver's sequence. That is `shared_desugars()` in `main.rs`, opened by
`build`, `check`, `run`, `emit-ir`, `emit-wat`, `emit-lowered`, `test`, `bench`,
`serve` and `dev`.

**Opened AFTER the load, and that is a decision rather than a detail.** The
loader runs generators (RFC-0021) by loading and checking whole programs of their
own and throwing them away. A memo keyed by node address over a program that dies
is the leak the `Memo` doc already warns about, and it is worse than a leak: a
freed node's address is handed out again, so a key from a dead generator module
could be answered for a live one, and the `(receiver, arguments)` verification is
then the only thing between that and a miscompile. Opened after the load, every
key in the map is an address of the one linked program, which is alive for the
whole compile. **The one program the command is about, from the point it is
linked to the point it has been lowered and emitted.**

**What it found is a live defect, and it is the corpse hazard M2d wrote down.**
`vyrn_lower::lower` runs `checker::record` (§3 M1, deviation 6), which types each
projection expansion where it is inlined (M3b). Without a memo that checker
builds one leaked tree and the lowering's own walk builds another — and the
second one asks `Recorded` for a type at an address the first one's dead tree was
freed from. So a released `vyrn emit-lowered` has been printing types off dead
nodes since M2d. On `examples/projection.vyrn`:

```text
- var w : String          ← the address's previous tenant
+ var w : Window
- field .start : Float64
+ field .start : Int64
```

**10 of 163 examples' dumps change, 1,354 lines.** M2d's own hazard note said
"any future consumer that looks a node up in `Recorded` by an address it did not
get from the program can be answered by a corpse. It is written here because the
next milestone is exactly such a consumer" — and the next milestone was, in the
gate, where the memo was open. Nobody checked the CLI. `projection.lowered` is a
fifth blessed dump, added because the other four reach no projection and so
nothing held this.

**Byte identity: 326 of 326 identical, raw**, both backends, `emit-ir` and
`emit-wat`, run from `examples/` with a relative filename. **No temp-counter
renumbering, and the reason is worth the line rather than the relief**: a
released `vyrn build` runs exactly ONE expander after the load, so the memo has
nothing to share with and `project::inline`'s tag counter advances the same
number of times it always did. Measured directly, with a hit/miss counter:
`emit-ir` and `emit-wat` are 9 misses and 0 hits on `projection.vyrn`; `check`
and `emit-lowered` are 9 hits and 9 misses, because those two run the lowering
and the lowering runs the checker. **So "all engines read the shared expansion"
is true of the two commands that lower and vacuous for the ones that only emit**
— the sharing that matters today is checker-to-lowering, and backend-to-backend
sharing arrives when a backend has a reason to read a recorded type, which is
M3's delete half. The brief for this phase read as if a build were already
sharing three ways; it never was.

**Compile time is noise.** Five runs each of `check` / `emit-lowered` /
`emit-ir` over the four projection-heaviest examples: every pair is within 3 ms
of the other on 36–75 ms totals, and process start dominates both. One expansion
instead of two is real and too small to see.

#### Piece 2 — the attribution M6a owed, and the class it did not know about

M6a closed by saying the instrument existed and the attribution did not: "which
of the 299 are inside a lifted body and which are not". `observe::Row` carries
the answer now — not a flag but the CLONE the answer was given inside, because
measuring the first one found a second.

| | count |
|---|---|
| off-program answers (`Tally::synthesized`) | 2,035 |
| …given inside `Fn_::lift_lambda`'s cloned body | **532** |
| …given inside a cloned `where` predicate | **1,043** |
| …neither | 460 |

and `peek`'s 299 split **68 lambda / 122 predicate / 109 neither**.

**The predicate clone is new to this file and it is twice the lambda's.** A
`where` predicate lives on a `TypeDecl`; `types::decl_map` copies every
`TypeDecl` into a `HashMap` for each engine, and then each validation site copies
the predicate again — `decl.predicate.clone()` at `lib.rs:11655` and
`direct.rs:4169`, both to get past the borrow checker. Two levels of copy, at
every value boundary a refined type crosses, in all three engines. §1.5 examined
this exact code path and concluded that validation "is what a shared decision
looks like after it has been shared", which is true of the ORDER the predicates
run in and false of the tree they run over.

**The way this was found is the method this file keeps recommending, applied to
itself.** M6a named `value` — "the binding a scalar `where` predicate uses" — and
reasoned that a predicate is not inside any function instance, so the form cannot
hold it. That is a gap in what the form carries. The first version of this phase
gated on it: the corpus gate collected every node address inside every
`TypeDecl`'s predicate and tagged the residue against them, and **zero rows
matched**. The addresses do not match because the backends never walk the
program's predicate. A hypothesis that priced a bucket was refuted by an
address comparison, and the second one was measured rather than argued.

**`Fn_::lift_lambda`'s clone is not load-bearing, and the textual backend is the
proof.** `emit_lifted_lambda` (`lib.rs:7948`) does the same job by walking the
literal's own `LambdaBody` — captures become leading parameters of a SIGNATURE,
which is where the prepending lives; the body is never copied. The direct
backend's clone is `b.clone()` and `(**e).clone()` at `direct.rs:7986`, and its
`Pending` already carries `body: Option<&'a Block>` for exactly this purpose
(M2c). The `LambdaBody::Block` half could borrow today. The `LambdaBody::Expr`
half needs an owned `Stmt::Return` wrapper, and M2d already shipped the shape
that answers it: leak one, key it by the lambda's node address, let the checker
type it in the caller's scope, and have the lowering walk it. So the clone can
stop being a clone, at about the price of M2d-b.

**It was measured and NOT done, and the measurement is why.** The move buys 532
of 2,035 residue rows and takes `peek` from 299 to 231. It does not make `peek`
deletable, which is the only thing M3's `−1,200` was ever waiting on. Its stated
need was that gate; the gate closes below for an unrelated reason, so the move is
a cleanup with a byte-identity risk in `Fn_::expr` and no gate riding on it. The
price and the payoff are written here so a future phase with a reason does not
have to find them again.

#### M3's gate — CLOSED, permanently unmet, and the terminal reason is §2.3

M3 asked for `peek` (510), `peek_arm`, `peek_ho`, `gen_peek`, `match_ty`, `join`,
`static_ty` and both `expect` stacks to be deleted, for at least **−1,200 lines**
across the two backends. It has been re-parented onto M4, then M5, then M6, and
M6a refused a fifth. **This phase closes it instead, and the number is 109.**

Every sharing move this arc has left available — the lambda body (68) and the
predicate (122) — still leaves `peek` answering **109 questions at nodes no form
can hold**, and they are not a residue waiting for a mechanism:

| what | count | why no form reaches it |
|---|---|---|
| `Peek/var[@rel]` | 26 | the release receiver the backend parks in its own locals table. M6a read all ten such sites and ledgered them closed by §2.3: "the type is a fact about a wasm local, not about a program". |
| `Peek/call` | 48 | a call the emitter builds at an emit site to reach a dispatched method — the same sentence one level up. |
| `Peek/int`, `Peek/var[i]`, `Peek/var[s]`, … | 35 | operands of those, built beside them. |

**A `peek` that must answer 109 questions cannot be deleted**, and §2.3 assigns
every one of them to the backend on purpose — locals are the encoder's, and an
encoder that could not type its own local would need the form to hold a fact
about a wasm local, which is the opposite of what this RFC is for. What is
available is not a deletion but a SHRINK: `peek` reading the recorded type and
falling back to `lookup` for its own locals is one type mechanism plus the
encoder's own table, not the two §1.2 exists to remove. **That is a different
milestone with a different gate, and it should be written with a number under it
rather than inheriting a −1,200 that was priced before any of this was
measured.** The gate line is closed here; §3 M3's ledger stands as the record of
what it asked for.

#### Piece 3 — the `coerce` shadow is NOT started, and its step 0 says §1.5's correction did not go far enough

M6a corrected §1.5 from "505 lines of the same decision" to "344 lines of one
decision and 161 of a different one", on the ground that the interpreter's
`coerce` has no `from`. **Reading the two compiled ladders against each other
before writing the shadow says the 344 is not one decision either.** Rung by
rung:

| | native (`lib.rs:2611`) | wasm (`direct.rs:3912`) |
|---|---|---|
| substitute a `Param` first | — | yes, and its comment says a `Param` "would silently be neither `Named` nor a boundary" |
| `Never` | identity | identity |
| validation | coerce to base, then check | the same, **and the RFC-0020 containment skip is inside this rung** |
| the containment skip | a **separate function above the ladder** (`coerce_flow`), reachable only from a site that has the `Expr` | a parameter of the one function (`expr: Option<&Expr>`) |
| `Fn` ↔ `Fn` | an explicit retag rung | none — it reaches the `ll`-equality shortcut |
| `ArrayN` → `ArrayN` | element-wise, so each element validates | none |
| numeric | one rung: truncate into an `IntN`; extension lives in the loads | `Num::of` on both sides, dispatched on (equal, source wide, target wide), widening by the SOURCE's signedness and renormalising into the TARGET's |
| int ↔ float | one rung: `Float` → `Float32` | every crossing and both float widths, `trunc_sat` |
| an `ll`-equality shortcut | none | yes, and the numeric rung must precede it — `llt` prints `i8` for `Int8` and `UInt8` alike |
| array reshapes vs numeric | reshapes FIRST | numeric FIRST |
| the end of the ladder | **identity** | **a refusal** (`a conversion from X to Y`) |

So the two are not one plan with two vocabularies. They are two plans that agree
on the first two rungs, order the middle differently, and disagree about what an
unhandled pair means. **A shadow that computes "the ladder's decision" and gates
both backends against it has to decide which of the two orders is the decision
before it can gate anything**, and that is a design question this phase did not
have the budget to answer well.

**One difference was written as a program, in the M4 step-0 method, and it came
back green.** The `ArrayN → ArrayN` rung native has and wasm does not predicts
that `let xs: Array<Age, 2> = [n, n]` with a runtime `n` below `Age`'s bound
traps on native and passes on wasm. All three engines print
`error: validation failed for `Age`` and exit 1: the elements are coerced into
`Age` where the literal is BUILT, on both backends, so the ladder rung is not the
only path to that check and the difference is unreachable from there. The
prediction failed and is recorded rather than dropped — three of the four
predictions this arc has written as programs found a defect, and a method that
only gets written up when it wins is not a method.

**The shape the shadow should take, so the next phase does not re-derive it.**
Two engines, not three (§2.4 row 4). The named rules go on the ORDER — one rule
per rung a backend takes that the plan does not place, one per rung it takes in a
different position — because the four differences above are all order, not
vocabulary. The terminal rung is the first thing to gate and the cheapest: a pair
native passes through and wasm refuses is a compile error on one target only, and
nothing today asserts the two agree about which pairs are unhandled. Break it in
both directions, as M4's placement gate does.

#### The gate, and the verdict

**184 changed lines across 5 compiler files** — 159 added and 25 removed —
against §3.0's ≤ 800 / ≤ 15, plus a 248-line blessed snapshot (generated output,
counted apart) and this text. The residue axis is 58 of the 184 and the driver's
memo is 45.

**Byte identity.** All 163 examples, both backends, `emit-ir` and `emit-wat`
hashed against `main` at `8502b67`, run from `examples/` with a relative
filename: **326 of 326 identical, raw.** Nothing in either emitter's decision
path changed — `observe::set_ctx` is a thread-local write either way and the
rows it marks are read by the gate alone. Full parity green (40 tests,
`--ignored`), workspace green (68 suites), `vyrn-lsp` green (76),
`cargo fmt --check` clean on all three manifests.

**M3's gate is closed** (above, with 109). **M5's `coerce` phase is still not
started**, and this phase's contribution to it is the rung table rather than a
line of code. `Tally::synthesized`'s ceiling is unchanged at 2,100; the count
still drifts run to run (1,926 to 2,059 across three runs here) for the reason
§3 M2 finding 4 records, and the halves that matter — the three clone contexts —
are stable.

#### Three places this phase's brief did not survive contact with the code

1. **"Both backends walk each other's nodes for the first time" — they do not,
   and no released command makes them.** The brief predicted that as production
   sharing's cost. A `vyrn build` runs one backend; nothing in the CLI runs two
   in one process. What the memo actually joins is the checker inside
   `vyrn_lower::lower` to the lowering's own walk, which is why the visible
   change is `emit-lowered`'s types and not a renumbered temporary.
2. **The brief's two branches for `lift_lambda` were "the clone can stop being a
   clone" and "the clone is load-bearing". It is the first, and the gate closes
   anyway.** The clone is removable — the textual backend proves it by not having
   one — and removing it moves `peek` from 299 to 231, which is not deletable
   either. The verdict M3's gate needed came from a class the brief did not name.
3. **The instrument the brief asked for measured one clone and found two.** It
   was written as `lambda: bool` and shipped as `ctx: &'static str`, because the
   first run of the predicate hypothesis returned zero and the only way to tell
   "not a predicate" from "a COPY of a predicate" was to ask the engine rather
   than the addresses.

### M6 third phase — BOTH CLONES ARE DELETED AND THE OFF-PROGRAM RESIDUE IS 1,052, OF WHICH `peek`'s 110 IS THE §2.3 CLASS AND HAS A FLOOR NOW AS WELL AS A CEILING. THE `coerce` SHADOW IS BUILT AND GREEN: FOUR NAMED RULES, ALL OF THEM ORDER, AND THE TERMINAL RUNG GATED FIRST. AND THE LAMBDA CLONE WAS HIDING A DEFECT NOBODY HAD PRICED — THE WASM BACKEND NEVER FREED A CALL-ARGUMENT TEMPORARY INSIDE A LAMBDA BODY, BECAUSE `own`'s ANSWER IS KEYED BY NODE ADDRESS AND IT WAS WALKING A COPY

| PR | What | Changed lines | Files |
|---|---|---|---|
| M6c | the direct backend queues the literal's own nodes; both backends read the program's own `where` predicate; `vyrn-lower` records it; the counters | 616 | 5 + a pointer line here |
| M6d | `Rung` and `coerce_plan`; both ladders say which rung they took; the corpus gate, its four rules, its breakage tests; this text | 463 | 3 + this file |

#### Piece 1 — the lambda clone, and the leak under it

`Fn_::lift_lambda` copied the body (`b.clone()` and `(**e).clone()`) so the
worklist had something to hold. **The obstacle was never the clone; it was the
lifetime.** `Fn_::expr` walks `&Expr` with the program's lifetime erased, so at a
literal the backend has the node and no borrow a `Pending` can keep — which is
why M2c could borrow a generic callee's block (it comes from `&'a Function`, off
the worklist) and could not borrow this one.

**The brief's remedy — thread the program's lifetime back through the walk — was
tried and measured, and it costs more than it buys.** The signatures take it
mechanically — the compiler names each one, and each round of naming ends — and
then **13 sites stop compiling, every one of them a tree the emitter builds ON
PURPOSE**: the receiver it parks to reach an implicitly
dispatched `release`, the capture-source lists an RFC-0023 call assembles, the
`toJson`/`fromJson`/`schemaOf` rewrites. Making those satisfy the program's
lifetime means leaking each of them — an unbounded leak in the LSP, which
compiles a generator per keystroke — to delete a bounded one. **`ast::lambdas` is
the borrow without the leak**: one walk over the program indexes every literal by
address, and a hit IS the program's node, because the program outlives every walk
over it and nothing else can be living at one of its addresses. No verification,
which is what separates this key from `project::Memo`'s.

`Pending::body` is a `Body` now — `Block`, `Value`, or `Shell` — and the
`LambdaBody::Expr` half needs no owned wrapper at all: `Fn_::lambda_value` emits
the value and the branch that `Stmt::Return`'s arm emits, without a statement
built to hold a copy of `e`. `Shell` is what is left for a literal the program
does not hold, and the corpus reaches none.

**What it found is a wasm-only leak, and the byte difference is that fix.**
`own` marks every call-argument temporary the caller reclaims, keyed by NODE
ADDRESS; a backend walking a copy of a lambda's body finds no key, so
`each(xs, |n| print("plain: \{n}"))` allocated an interpolated `String` per call
and freed none of it. The textual emitter has always freed it — it walks the
literal's own nodes — and `examples/fnvalstore.vyrn`'s native IR is unchanged.
**`memory.rs` did not catch it because it is not a corpus test**: it runs its own
fixtures, and none of them has a lambda that allocates. That is the third defect
this arc's clones were hiding, and the second one that only a change of ADDRESS
could expose.

#### Piece 2 — the `where` predicate, and where the count actually moves

`types::decl_map` copies every `TypeDecl` per engine and each validation site
copied the predicate again. Both backends read the program's own node now
(`Cx::decls`, `Gen::decls`), by name and verified by the base, refusing rather
than silently skipping if the decl is not the program's.

**And that alone moves nothing, which is the finding.** Sharing the tree makes
the addresses the program's; it does not make them the FORM's, and the residue
counts answers about nodes no instantiation of the program holds. The 1,043 only
die because `vyrn-lower` records each predicate's types — a third root beside the
instances and the module-state initializers, and the one with no worklist
attached: a predicate's calls are deliberately not followed, because nothing
walked a predicate before this and following them would invent instances the two
backends' own worklists do not have.

**A predicate is outside every instantiation, and the gate had to be told.** It
lives on a declaration, which has no type parameters, so an answer about one is
looked up with the instantiation dropped. Without that rule the 1,043 would have
moved from `synthesized` to `uninstantiated` and the milestone would have
reported a win it had not made.

#### Measured

| | before | after |
|---|---|---|
| `Tally::synthesized` (off-program answers) | 1,955 | **1,043–1,052** |
| …inside `lift_lambda`'s clone | 532 | **0**, asserted |
| …inside a cloned `where` predicate | 1,043 | **0**, asserted |
| `peek`'s share | 299 | **110** |
| `Tally::compared` | 588,793 | 589,839 |
| `unrecorded` | 78 | 78 |

**The two clone counters are asserted at ZERO rather than bounded by a ceiling,
and that is a stronger gate than the number it replaces.** Each engine marks the
rows it gives inside a tree it copied; a copy's addresses are not the program's,
so a clone that comes back lands in a counter that must be zero rather than in
one that drifts by ±200 a run. The ceiling on the total falls 2,100 → 1,200.

**And `peek`'s 110 gets a FLOOR.** §2.3 assigns every one of them to the backend
on purpose — the type of a release receiver, of a dispatched call the emitter
builds at an emit site, and of the operands built beside them — so both bounds
fail loudly: a rise means a new engine-built tree, a fall means §2.3 moved and
this file needs re-reading. M6b closed M3's gate with this number; it is pinned
now rather than remembered.

#### Piece 3 — the `coerce` shadow, and the four rules are all ORDER

`Rung` and `coerce_plan` are the vocabulary and the plan; every `return` path of
both ladders says which rung it took; the corpus gate holds 204,539 crossings to
the plan. **The plan is beside `validation_required` and `llt_of` rather than in
`vyrn-lower`, because it is made OF them** — a deviation from this phase's brief,
and the same shape as M5's answer to §6.4: a shared decision goes where its
inputs already are, and a plan keyed on a pair needs no site.

| | count | the rule |
|---|---|---|
| took the planned rung | 126,367 | — |
| `Wasm`: plan `Identity`, took `Resize` | 75,420 | `NumericBeforeShape`. Its numeric rung answers for EVERY integer pair, equal or not, and it must: `llt` prints `i8` for `Int8` and `UInt8` alike, so a shape shortcut reached first would swallow the one pair whose bits genuinely move. **This is §1.5's order difference, and it is the only place the order is observable at all** — every other middle rung's guard is disjoint from the others'. |
| `Native`: plan `Identity`, took `Resize` | 2,474 | `SizedTargetRung`. Its resize rung is guarded by the TARGET being a sized integer rather than by the pair differing, so `Int16 → Int16` takes it and emits nothing. |
| `Wasm`: plan `FnRetag`, took `Identity` | 224 | `FnByShape`. It has no function rung: the structural spelling and every named alias share `{ i64, i64 }`, so the shape shortcut answers first. |
| `Native`: plan `Refuse`, took `Identity` | 54 | `ParamSpelling` — below. |
| no rule | **0** | |

**The terminal rung is gated first and separately, because it is the one
difference that is a program compiling on one target only.** The two ladders end
opposite ways: the textual one reinterprets the bits under a new name, the direct
one refuses. Every pair that reaches the textual one's end in the corpus has a
type PARAMETER on one side (`String → T`, `P → { id: Int64, name: String }`,
`T → CursorCell`) — the direct ladder substitutes before its first rung and the
textual one does not.

**That looked like the third live parity hole, and two programs say it is not.**
If a value could flow into a `T` the receiver had fixed at a refined type, the
textual backend would skip the validation the direct one runs — a trap on one
target only. It cannot: the checker rejects it, with a bare generic
(`firstOr(ages, n)` where `ages: Array<Age>` and `n: Int64`) and with an
associated type (`Option<Age>.valueOr(n)`) alike, both refused as "type parameter
`T` is both Age and Int64". So `from` and `to` at those 54 crossings are one type
under two spellings, and the identity is right. **The prediction was written as
programs and came back green, and it is recorded rather than dropped** — the M4
step-0 method, now 3 defects found in 5 predictions, which is what a method looks
like when it is reported either way.

**The gate is broken on purpose in both directions.** `rung_rule` is a pure
function and the tests hold it to its own edges: a refused pair with NO type
parameter has no rule (that is the hole, and nothing else in the repository asks
about it); the textual ladder skipping its own function rung is not the direct
one's shape shortcut; a validation skipped is never a rule. A green run over four
rules proves the rules FIRE — these prove they are narrow.

#### Byte identity

**325 of 326 identical, raw**, both backends, `emit-ir` and `emit-wat` over all
163 examples against `main`, run from `examples/` with a relative filename. The
one difference is `fnvalstore.vyrn`'s `emit-wat` and it is the leak fix above:
four instructions per lambda-body argument temporary, freeing what the textual
backend has always freed. Piece 3 adds no instruction anywhere — every
`note_rung` is behind the same thread-local the type rows are behind.

#### Four places this phase's brief did not survive contact with the code

1. **"Pieces 1–2 are mechanical by prior measurement." They were not, and the
   reason is the same for both.** M6b priced the lambda at "about M2d-b's price"
   from the textual backend's proof that no copy is needed. What the price
   actually is: a lifetime the walk has erased, whose restoration is measured
   above at 34 signatures and 13 deliberate leaks. And the predicate's price is
   not the sharing at all — it is the RECORDING, in a crate the brief did not
   name.
2. **"Share the checked predicate the way projections are shared" — the
   projection precedent is the wrong one.** A projection is leaked and memoized
   because it does not exist until an engine builds it. A predicate already
   exists, in the program, at a stable address; nothing needs leaking, and the
   memo/corpse hazard the brief warned about does not arise. What was needed
   instead was a reader in `vyrn-lower` and a lookup rule in the gate.
3. **"The lowering computes each coercion site's rung sequence."** It computes a
   rung, not a sequence, and it computes it from the pair rather than at a site:
   each ladder takes exactly one rung and recurses, so a "sequence" is one rung
   per pair with the nested crossings recorded as their own. And the plan lives
   below neither engine but beside the two decisions it is made of.
4. **"Look hard at the identity fall-through; if they diverge that is the
   headline."** They do not, and the headline came from the piece the brief
   called mechanical: the wasm backend was leaking every call-argument temporary
   inside a lambda body, and only a change of ADDRESS could show it.

---

## 4. What it unlocks

Stated briefly, because none of it is a reason to do M1.

- **A new backend is an emitter.** A Cranelift backend becomes representation,
  locals, encoding, a control-flow flattener (§2.2) and a runtime, against a
  lowered form that already decided the language. An ARM native target is the
  same shape. An alternate wasm engine is smaller still.

  **[A15] The 250 ms figure that used to be in this line is gone.** It cited
  RFC-0077's clang-against-cranelift table as if a fourth backend would buy that
  ratio. The published Rust measurement has Cranelift *slower* on incremental
  builds — 7.98 s against 5.48 s — and faster on a full debug build, 49.93 s
  against 54.64 s. Neither number transfers to Vyrn. §5 refuses to make a
  compile-speed claim, and §4 was making one.
- **A semantic change becomes a reviewable diff of the form.** [A13] After M6 the
  lowered form is the single artifact, so changing what the language *means*
  shows up as a diff of a checked-in dump inside the pull request, before anyone
  runs anything. This is rustc's `mir-opt` model, and its workflow guide tells
  authors to bless and commit the dump *before* implementing an optimization, "so
  that you (and your reviewers) can see a before/after diff of what the
  optimization changed". Snapshot **ten** examples with a `--bless` flag, not 161:
  ten small snapshots are read, and 161 large ones are skipped. This is the payoff
  that pays on every pull request, and the first draft of §4 omitted it.
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
  engine, an encoder — and that is genuinely new work per target. QBE is the
  price list: about 8 kloc buys a whole optimizing backend for three
  architectures, register allocation and the C ABI included. That says the ~5,400
  lines §1.7 estimates for Vyrn's residue is the right order of magnitude, which
  is a confirmation of the cost, not a discount on it.
- **It does not touch the bugs already in the shared half.** `own.rs`'s fates,
  `layout.rs`, `predicate_binds` and the synthesized codecs are shared today and
  can still be wrong.
- **Six milestones and twelve pull requests is a long arc against a live corpus.**
  The memory-model arc ran ten phases and eighteen PRs, and every phase corrected
  its own brief. This one should be expected to do the same, and the corrections
  belong in this file. Fifteen of them arrived before M1 started, from
  `docs/research/lowering-design.md`, and they are marked [A1] to [A15] above.

---

## 6. Open questions

Five were asked. Four are now answered — 6.1 by the migration, 6.3 and 6.4 by
milestones, 6.5 by rustc — and each keeps its question here rather than
disappearing into the design, so a reader can see what was decided and against
what. 6.2 is the one still open.

### 6.1 Does `Lowered` own or borrow? **Answered: borrow, during the migration — and M1 measured what the other one costs.**

A form that borrows the `Program` is cheap and pins the AST for the whole build;
one that owns is a second copy of every body, per instantiation. This was left
open. **[A6] The migration decides it before performance gets a vote.** A
borrowed node can carry the AST node it came from, and that is the only thing
that lets an engine migrate one arm at a time and fall back to its old walk for
the rest. Without it, every delete-half PR is all-or-nothing per engine, M4
cannot split by exit kind and M6 has no legal intermediate state. **M1 measured
the owned version: +1.20 MiB against the borrowed form's 1.69 MiB on the largest
corpus module, 1.71× (§2.1 item 1).** So cost was never going to decide this
one — the migration did, and the number says the fallback is not needed at
anything like the scale this project builds. The question is revisited after M6,
when the fallback arms are gone and the only argument left is cost.
### 6.2 What happens to `movecheck` and `own`? **Open.**

Both key on node address in the AST today. If the lowering carries releases
explicitly, `own`'s placement output has no consumer left in the backends — but
`movecheck`'s diagnostics are about source the user wrote, and they must not
start naming lowered nodes. Swift hints at the answer without giving it: it keeps
diagnosis *before* canonicalization, which is where Vyrn already runs both of
these. A hint is not an answer, and this stays open.

### 6.3 Where does the loader's generated code enter? **Answered by M1: nowhere special.**

Generators produce Vyrn source that is parsed, checked and lowered like any other
module (`loader.rs:1570`), so it should need no special case. **M1's corpus gate
lowers every generated module the corpus links — `graphql.vyrn` alone is 372
functions, most of them generated — and needed no arm for any of them.** The one
special case M1 did find is a synthesized one rather than a generated one: the
JSON codecs `check_and_synthesize` adds AFTER it checks are never typed by that
pass, which is why the lowering records over the program it is given (§3 M1,
deviation 6). Note the comptime sandbox is a fourth consumer of the
form, by both routes: a `gen fn` runs through `interp::generate` or, since
RFC-0076, through compiled wasm, and both routes are Vyrn programs.

### 6.4 Can `vyrn-lower` be a module of `vyrn-frontend` instead of a crate? **Answered by M5: it stays a crate, because the trap table went below it rather than into it.**

Lazier, and it puts the trap table where the interpreter can already reach it.
The argument for a crate is only that `vyrn-frontend` is already 13,526 lines of
checker; that is not much of an argument. M1 shipped the crate because a crate
boundary is what stopped the lowering from reaching a backend by accident, and
because moving a module later is a rename. **M5 settled it, and not the way the
question expected**: the trap table is the first thing the interpreter must
import, and `vyrn-frontend` cannot depend on `vyrn-lower` while `vyrn-lower`
depends on the checker — so the table went into `vyrn_frontend::trap`, which is
the crate all three engines already read. Nothing had to merge. That is the same
seam M4c used for the release placement (`own`), and using it twice is what makes
it a rule rather than a workaround: **what three engines share lives in the crate
three engines depend on, and the form is what reads it.** The crate boundary keeps
the argument M1 shipped it for.

### 6.5 Does the form need a stable text rendering? **Answered: no. Print, do not parse. Unstable text, blessed snapshots.**

**[A12]** The worry was real — "a rendering people read becomes a rendering
people depend on" — and rustc has already answered it in a comment. Every MIR
dump carries "subject to change without notice. Knock yourself out.", and
`tests/mir-opt` blesses `.mir` files with `--bless` anyway. **Stability is
enforced by a blessed snapshot suite, not promised by a contract.** A format
change becomes one wide, reviewable diff instead of a compatibility argument.
`emit-lowered` prints a version line (`; vyrn lowered v1`), promises nothing, and
its snapshots are blessed.

**And there is no parser.** MLIR's textual round-trip is the tempting version of
this, and it serves a pass pipeline that reads back what it wrote. Vyrn has one
pass. A parser for the lowered form would be a second front end, written to test
a printer, and it would be the largest single piece of new code this RFC could
accidentally acquire. Print deterministically, check the print in, diff it. That
catches everything the round-trip catches except "the printer and the parser
disagree", and there is no parser to disagree with.
