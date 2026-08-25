# RFC-0114 — Ownership At A Program Point

- **Status:** **Implemented** — the design the appendix proves, landed as it
  was proved: M1 argument temporaries (313.9 MB → 4.1), M2 per-store
  ownedness (9,925.7 MB → 4.9), Rule N at all three join shapes (215.3 MB →
  one buffer each), R1′ receivers for Strings and containers (313.9 → steady;
  178.9 → 3.8), the untake (423 → 4.1), and the consume-parameter release the
  untake's own measurement uncovered. Every landing is pinned by a
  `memory.rs` row that failed first. Still out, recorded in the census: a
  heap field of a temporary record, and the §26 `ReleasePlan` consolidation.
- **Evidence:** [rfcs/census/declared-release-does-not-run.md](census/declared-release-does-not-run.md).
- **Appendix A:** [the algorithm and its proofs](proofs/release-algorithm.md) —
  Part I: the invariant, three soundness theorems, and the assumption each of
  today's defects violated. Part II: the complete model — edge normalization
  makes the ambiguous join UNREACHABLE instead of refused, with closure and
  optimality theorems (minimal releases, zero runtime state, pointwise-minimal
  residency) and the one case it cannot serve. Part III: the unconstrained
  redesign — static ownership as partially evaluated refcounting (Theorem 8),
  the refusal set proved minimal (Theorem 10), one `ReleasePlan` artifact that
  deletes six decision mechanisms from both backends, witnessed classification,
  and free-trace bisimulation against the interpreter as the standing gate.
  Part IV: foundations — the instrumented machine with a ghost billing map, the
  bracketing lemma that completes Theorem 8, the Galois connection constructed
  with COMPLETENESS proved (Theorem 11: a runtime check is the price of the
  concretization gap, and Rule N closes the gap instead of paying it), the
  declarative linear system Ω with the dataflow as its elaborator (Theorem 12),
  a sharpness table giving each assumption its two-line counterexample, the
  manager-space theorem (the discipline makes the offline optimum
  online-achievable at zero cost — Theorem 13), and the mechanization roadmap.
  Part V: structure — the Heap Forest Theorem (under the move discipline the
  heap has the same shape as the context, and every invariant is forest
  preservation), the place algebra with RFC-0093's holes as its implementation,
  a trip-count-independent frame-live bound, release placement as a confluent
  rewriting system whose unique normal form is the earliest plan, task
  partition from spawn isolation, and borrows/lifetimes/RFC-0109 unified as one
  question about brackets: who proves the nesting.

## The gap in one sentence

The compiled backends know whether a BINDING is released at the end of its
block; they do not know whether a PLACE holds heap at a given statement — so a
value that is neither a live binding nor a block-exit binding is never released.

## Why this is one RFC and not two bugs

Two leaks are open, they look unrelated, and they are the same missing fact.

**A temporary has no binding.** `check(make(depth))` builds a tree, lends it to
`check`, and drops it on the floor. `Gen` emits a release from `drop_slots`,
which is keyed on `let` bindings, and a temporary is not one.

**A binding whose last value escapes is treated as owning nothing for its whole
life.** `Gen::slot_owns` asks whether the slot is in `drop_slots`. A binding
consumed into a record at the end is not in it — so no assignment ANYWHERE in
its life releases what it replaced, and every intermediate leaks.

Both are the same question asked of the wrong thing. `drop_slots` answers "will
this binding be released when the block ends". Neither leak is about the end of
a block.

## The measurements

All native unless stated, peak working set.

| program | shape | peak |
| --- | --- | --- |
| `check(make(8))` x 20,000 | temporary, **interpreter** | **8.5 MB** |
| the same program | temporary, **native** | **313.9 MB** |
| binary-trees, depth 16 | temporary | 451.9 MB |
| the same, one `let` added | bound | **20.5 MB** |
| 200-iteration concat x 50,000 | `out` returned | 4.2 MB |
| the same loop | `out` consumed into a record | **9925.7 MB** |

The first two rows are the same program with the same output — `10220000` —
differing by 37x in memory.

## THE INTERPRETER IS RIGHT AND THE COMPILED BACKENDS ARE WRONG

That is the part that makes this urgent rather than merely untidy. `Val::Str` is
an `Rc<String>` and `Val::Array` an `Rc<Vec<Val>>`, so the interpreter reclaims a
temporary when the last handle goes, with no analysis at all. Native and wasm
carry no refcount and rely on the analysis, and the analysis is not asked.

**Three-way parity cannot see this.** It compares bytes on stdout and exit codes,
which are identical. A test suite that runs every example on three engines
reports 40/40 while one engine uses 37 times the memory of another.

## What a design has to satisfy

1. **No double free.** This is the whole difficulty. A binding may be assigned
   again after its value moved out; a temporary may be the same buffer the callee
   kept. Freeing a place whose value is already gone is worse than the leak.
2. **Three-way parity, in memory as well as in bytes.** Whatever lands, the
   measurement above becomes a test — `compiler/vyrn-cli/tests/memory.rs` is the
   harness that already runs shapes on wasm and asserts steady against leaking.
3. **No new cost where nothing owns heap.** A program of integers must emit
   exactly what it emits today.

## What is already known

The move checker HAS the fact. `movecheck` computes per-binding fates
(`Fate`, `Gone::Moved`, `Gone::Returned`, …) and the diagnostics quote them at
statement granularity — "`h.data` may not be returned", "`key` is consumed by `m`
inside a loop". What does not exist is a channel from that analysis to codegen
that answers "at this store, does this place hold something the frame owns".

Two fixes landed the same day this was written, and both were the narrow,
provable half of a wider question:

- `own::owns_heap` answered `false` for a self-referring type because it ran out
  of a depth counter. A cycle is now `true`, and 200,000 recursive values went
  from 3.1 GB to 3.8 MB.
- The store's release was skipped whenever the new value mentioned the place. A
  String `+` always allocates a fresh buffer, so that case now releases; a
  prepend loop went from 9.9 GB to 4.5 MB.

Neither touched the question above, and neither could: both were about a value
the frame already knew it owned.

## The candidate directions

`RECOMMENDATION, NOT A DECISION.` Each is a direction, not a specification.

| direction | what it adds | double-free risk | cost where nothing owns heap |
| --- | --- | --- | --- |
| **Statement-scoped temporaries** — a call whose return type owns heap, in argument or discarded position, is released after the enclosing statement | a per-statement list of owned temporaries in `Gen` | low: a return IS owned, which the language already refuses to let be a borrow | none — the list is empty |
| **Per-store liveness** — the move checker publishes, per assignment, whether the place currently holds an owned value | a table from `movecheck` to codegen, keyed by statement | low, and it is the analysis's whole job | none |
| **Refcount the compiled representations** — give `String` and `Array` the header the interpreter's `Rc` already is | one word per allocation | none | a word and an increment everywhere, which fails constraint 3 |
| **Accept it** | nothing | none | none |

The first two are complementary rather than alternative: the first closes the
temporary, the second closes the escaping binding, and both want the same channel
out of `movecheck`.

The third is what `Val` does and it is why the interpreter is correct here. It is
listed because it is the honest comparison, and refused for the reason
constraint 3 gives — the parity programs hold only numbers, and they are at
parity.

## A recommended shape

`RECOMMENDATION, NOT A DECISION`, but an opinionated one, in the order the two
halves should land.

### M1 — temporaries. The analysis is done; the EMISSION is the work.

This was scoped by trying it, and the answer moved. The channel is not missing.

`movecheck` already records every call-argument temporary with a verdict, and
`ArgVerdict::Released` is exactly the guarantee M1 wants: "a `read` parameter
that keeps nothing — the caller releases the temporary after the call; rules 2
and 3 refuse every way the callee could have kept it". The aliasing case has its
own verdict, `Lent`: "the result points into the argument". `Gen` already carries
`arg_frees`, and `gen_call` already releases everything pushed to it after the
call returns. `rfcs/census-call-arguments.md` is the record.

**One line stands between that and the fix**, in `Ownership::arg_drops`:

```rust
.filter(|s| {
    s.verdict == crate::movecheck::ArgVerdict::Released && s.kind == DropKind::FreeStr
})
```

`&& s.kind == DropKind::FreeStr` throws away every argument temporary that is not
a plain String — every record, every array, every declared release. That is why
`check(make(depth))` leaks a tree.

**Removing it breaks ONE backend, and measuring which is what scopes M1.**

The textual backend refuses to compile, deterministically:

```
ti3.ll:637:34: error: '%t6' defined with type '{ i64, i64, i64 }' but expected 'ptr'
  call void @__vyrn_str_free(ptr %t6)
```

`arg_frees` there is a `Vec<String>` of registers and the free site emits
`@__vyrn_str_free` unconditionally. It cannot be a WRONG free: `Type::Str` is the
only type in `llt_of` that lowers to a bare `ptr` — every other owning type is a
struct, a scalar or a vector — so LLVM's type checker catches every case at
compile time. The filter is what keeps that emitter from being handed something
it has no code to free.

**The direct wasm backend needs nothing.** With the condition removed it builds,
runs, and frees correctly:

| `check(make(8))` x 20,000, wasm | peak |
| --- | --- |
| with the `FreeStr` condition | 330.5 MB |
| without it | **10 MB** |

Ten against the interpreter's 8.5, and the program prints `10220000` either way.
Checked across the corpus rather than on one program: all 158 examples built to
wasm both ways and run, output compared byte for byte. **Three differ, and none
of them behaviourally** — a timestamp in `clock`, the module PATH inside
`externdemo`'s error text, and `storage`'s random temp-file suffix.

So M1 is smaller than it looked, and it is one backend:

1. In `vyrn-codegen/src/lib.rs`, `arg_frees` carries the `DropKind` beside the
   register, and the free site dispatches on it instead of assuming a String.
   `snap_old` + `free_snap` is the existing pair, and it works from a SLOT — an
   argument temporary is an SSA value with no address, so the buffer extraction
   has to become reachable from a value.
2. Then drop the `FreeStr` condition, and both backends have it.

`direct.rs` is already correct because its `arg_frees` holds an `i32` local and
an aggregate's value there IS the pointer to its storage; `free_str_temp`'s
header adjustment lands on the same allocation the aggregate came from. That is
worth stating rather than relying on: it is why the wasm column moved without a
line of backend change.

**Released after the call, which `gen_call` already does.** Statement-end is the
looser alternative and is not needed here — the verdict is per argument position,
and the callee is done with the borrow when it returns.

**A correction, recorded because it was stated the other way first.** An earlier
draft of this section said dropping the condition was "a compile error at best
and a wrong free at worst". The second half is not supported: `Type::Str` is the
only bare `ptr` in `llt_of`, so the textual backend cannot silently mis-free, and
the wasm backend was measured over 158 programs with no behavioural change. The
risk in M1 is a build that stops compiling, which is the failure mode a compiler
is allowed to have.

### M2 — per-store liveness, and no drop flags

**Straight-line half LANDED.** `movecheck` records an event stream per binding
— every write (with whether the stored value was owning) and every take, each
stamped with walk order and the loops it sits in. `own::fold_store_owned`
folds it into the set of `Assign` nodes whose store releases the old value:
the previous write was owning, no take sits between the two in walk order,
and no take shares a loop with the store (a back edge makes walk order
meaningless inside one loop, so a shared loop refuses — refusal is the leak
direction, which is exactly the old behaviour). A binding whose final verdict
says somebody else holds the value — borrowed, lent, captured, aliased,
holed — releases at no store; `Moved`/`Dropped`/`Returned` are ordinary
takes, placed in the order, and do not veto. Both backends gate the store
snapshot on this one set, keyed by statement address, and on nothing of
their own; the old per-binding `slot_owns`/`place_owns` gate stays only for
field and element stores, which this slice does not touch.

Measured on landing: the escaping accumulator (50 000 rounds of an 8-concat
loop whose result is consumed into a record) fell from 9 925.7 MB to
4.9 MB native. `escapingAccumulator` in `memory.rs` flipped to `Steady` the
way the harness is built to flip — the row failed as "now reads Steady" and
was then rewritten. One codegen unit test moved with it, and the movement is
the fix observed from another angle: the copying string-accumulator lowering
now carries exactly one more free than the in-place lowering — the release
of the replaced value, which the in-place path has no counterpart for
because it reuses the buffer. The intermediates that free reclaims were
leaked before, equally, on both sides of that test's old equality.

**Rule N (the join half) LANDED for `if`.** The walker records every join
where exactly one branch consumed a whole binding — clean take, no hole, both
branches continuing, the binding untouched before the `if` and on the other
branch. `own::fold_edge_releases` keeps a candidate only when every write
into the binding anywhere is owning (a binding ever assigned a projection may
hold a borrow at the edge, and no walk-order argument survives a back edge)
and the final verdict is a plain take. Both backends then release at the end
of the non-consuming arm, growing an else-arm when the implicit edge owes the
release. The loop case needs no rule of its own: a binding declared outside a
loop and conditionally consumed inside it is already refused by the checker's
next-iteration reuse rule, so any candidate inside a loop is re-initialized
each iteration before its `if`. Measured: the conditional-move witness fell
from 215.3 MB to 3.8 MB over 200,000 rounds; `conditionalMove` in `memory.rs`
pins it. A declared `impl Owned` release stays off the edge (the thin refusal
below, unchanged).

**Rule N at `match` joins LANDED too**, with `edge` generalized to the arm's
source index. Two guards are specific to a match: the binding may be nobody's
binder (a binder shadows the name), and the scrutinee may not mention it (an
arm's payload projects into the scrutinee). A non-consuming arm releases only
when its value cannot alias the binding: no heap in the value, no mention of
the binding, or a `Binary`/`Unary` body — an operator result is a scalar or a
fresh concat, and the freshness witness in `interp.rs` executes that claim.
One implementation lesson worth keeping: `arm_carries_heap` must be asked
INSIDE the arm's scope, while the binder types exist — asked after the walk
it degrades to "yes" and the rule silently never fires. The match witness
measured the same as the `if`: ~210 MB to 3.8 MB. `conditionalMoveMatch`
pins it on wasm.

**Rule N at `if`-expression joins LANDED**, closing the third join shape —
the statement rule with the match's value guard and neither of its other
two (no binders, no scrutinee; the condition is a `Bool` read completed
before the branch). The value guard itself grew into a structural proof,
`value_cannot_alias`: an operator result is a scalar or a fresh allocation;
a scalar projection carries nothing; and a call cannot return heap it was
never handed, so its result is safe when every argument is — which is what
lets `Int64(s.byteLength)` release where a flat mentions-test refused it
(a lending function can only lend what it was passed; returning module
state or a projection is refused elsewhere; a capturing closure is
`Gone::Captured`, vetoed before the question is asked). The releases sit
under the branch value: stack-neutral in the wasm lowering, before the
branch to the `phi` in the textual one. `conditionalMoveIfExpr` pins it —
and its first run caught the flat guard failing, on wasm, before any code
shipped, which is the harness doing its job.

**R1′ LANDED for the pinned receiver shape.** A `.byteLength` read whose
receiver has no name is recorded by the walker (the field exists only on
String, which is the type proof), `facts` filters out lenders, and
`own::analyze` keeps the rows whose producer transfers ownership — a user
function returning String (the checker's "a return is owned" rule makes that
unconditional: returning a `read` parameter, a second name for one, or an
arm binder over one is refused at the source), or the fresh forms
`@concat`/`@str`. Both backends free the receiver right after the header
read — its last observer. `temporaryCall` flipped to `Steady` the way the
harness flips. The container half followed: `.length` on an unnamed
`Array`/`SmallArray`/`Map` a call produced frees the receiver — buffer and
elements — right after the count is read, filtered by the producer's return
KIND, so only silent frees ever enter the set (measured 178.9 MB → 3.8 over
200,000 rounds of a 128-element array; `temporaryArrayLength` pins it).
`.charCount()` needed nothing: it desugars to a call, and the M1 argument
machinery already frees its receiver-as-argument.

**The last receiver case CLOSED: a field of a temporary record.** The defect
was a classification: `names_a_place` called every field read "read out of a
place that owns it", but a field of a TEMPORARY is read out of a value
NOBODY owns — no row, no release, and calling the binding a borrow was the
leak. Two halves, split by what the field carries. A HEAP field transfers:
the binding owns the extracted buffer and its block exit releases it (safe
because a view builtin's field stays a borrow through the recursion, and a
user function cannot return a borrow at all — "a return is owned"). A
SCALAR field is the record's last observer: the record is freed whole,
deep, right after the read — with an aggregate field (an address INTO the
record), a `lazy` field (forced later), and any `Deep` producer in a
program that declares `impl Owned` anywhere staying out, each for its own
stated reason. Measured: 44.4 MB → 3.8 and 46.2 → 3.8 over a million
rounds; `temporaryRecordField` and `temporaryRecordScalar` pin both on
wasm, and both run clean under the free audit.

**The untake LANDED.** A binding whose value was taken and then provably
re-established releases its FINAL value at block exit — the naive version of
this was deferred as a double free, and the event stream is what makes the
non-naive version provable. Three refusals, each closing one double-free
path: the last event must be an OWNING write whose loop set and branch path
equal the `let`'s own (a revive inside a loop that may not run, or on one
arm of a branch, does not dominate the exit — the walker now stamps a branch
path on every event, mirroring the loop ids); every take must precede that
write in walk order; and no early exit — `return`, `?`, `break`, `continue`,
now recorded as walk-order positions — may sit between the first take and
the write, because on such an exit the binding still holds the taken state
and the exit path's releases run from the same `droppable` table. No backend
changed: the fold feeds `fate`, a qualified `Moved`/`Dropped` row becomes
`Reclaimed`, and all three engines already release a `mut` binding by its
slot's final value. The witness fell 423 MB → 4.1 MB; `revivedBinding` pins
it on wasm; the conditional-revive and early-return probes hold.

**The consume-parameter release LANDED** (it was found one commit earlier as
"half the untake witness's 423 MB"). A `consume` parameter whose type owns
heap gets a row keyed by its `Param` node, exactly as a `let` is keyed by
its statement — every take writes onto that row, so a param that is moved
on, dropped, or returned releases nothing, same as a `let`; a read-only one
is released at the callee's exit. Borrowed parameters stay at node 0, whose
reason stands (a row for one releases somebody else's value — the
`argsdemo` corruption). The placement puts owned params FIRST on the
outermost frame, so they release last; both backends register the drop in
the prologue; the interpreter seeds its body-block drops with them, so a
DECLARED release runs at the same point in all three engines
(byte-identical order verified). One exclusion, caught by the trace gate on
its own fixture before any program ran it: `release(consume self)` itself —
the release IS the release, and a row for its `self` would place a
self-recursive second one. `consumedParamRead` pins the fix on wasm; the
no-`drop` untake witness fell 194.4 MB → 3.8.

### The audit — §25's double-free half, standing

Every free the IR or the C shim runs now goes through one choke point,
`__vyrn_free`, and `VYRN_FREE_AUDIT=1` makes the allocator keep a
live-pointer table: a free of anything not in it — a double free, or a free
of memory the program never owned — prints one line and exits 134. A
`realloc` is audited as free-plus-malloc. Off (the default), the cost is one
branch per free, so nothing the benchmarks measure changes.

The peak rows see leaks; this sees the class they cannot, and it cannot be
fooled by a free at the wrong point that keeps peak flat. **The parity
harness sets it on every native run**, so the whole example corpus is a
double-free audit on every CI pass — 40/40 clean on landing. The mechanism
was proven the honest way: a hand-doctored IR with a duplicated
`__vyrn_str_free` runs to silent heap corruption without the audit and dies
loudly with it. The full §25 bisimulation (multiset equality of free traces
against the interpreter's Rc zero-crossings) remains open; this is its
cheaper, always-on half.

The state is per place, per program point: `Owned | Moved | Uninit`, a forward
dataflow with a join at every merge.

The design question is the join where one branch moved and the other did not.
Rust answers it with a DROP FLAG — a runtime boolean the release tests. **This
RFC recommends against that**, for two reasons that agree:

- It is a runtime cost at a call site that may own nothing, which constraint 3
  refuses.
- It is inference. This language's memory model is "ownership is DEFINED, not
  inferred" — the whole of RFC-0091 — and a conditional move that leaves a
  place's state ambiguous is exactly the shape it refuses elsewhere.

So: **refuse the ambiguity** — was this section's first answer, and Appendix A
Part II supersedes it with a strictly better one. Rule N (edge normalization)
releases on the branch that still owns, exactly when the place is dead after
the join; the ambiguous state then never arises, the program is ACCEPTED, and
the analysis stays exact with no flag (Theorem 4). The 211.5 MB conditional-move
leak becomes one buffer instead of a diagnostic. What survives of the refusal is
one thin case: a DECLARED `impl Owned` release moved on one path of a join —
user code whose timing all three engines must agree on, where an edge release
would change observable order and a flag would be inference. That case, and
only that case, is refused with a diagnostic naming the fix (`drop p` on the
non-moving path).

### How to make it reliable, which is the part today got wrong

**Three-way parity cannot see memory.** That is not a gap to work around; it is
the finding. Two leaks lived through 40/40 for as long as they existed, and the
`owns_heap` one survived a release mechanism written specifically to prevent it.

- `compiler/vyrn-cli/tests/memory.rs` is the harness that works. It runs a shape
  on wasm and asserts `Steady` against `Leaks` at two call counts. It caught,
  within one command, that a fix had been applied to one backend and not the
  other.
- **Every row of this work lands a row there first.** `prependLoop` was written
  before the fix, watched to fail, and then watched to pass.
- **Add the interpreter as a leg, and treat it as the ORACLE.** `Val::Str` is an
  `Rc<String>`: the interpreter is correct here by construction, with no analysis
  at all. So the expected shape does not have to be guessed — it can be measured
  from the engine that already gets it right, and the compiled backends compared
  against it within a tolerance. That is the check that would have caught every
  defect on this page.

### Performance

Nothing above costs anything where nothing owns heap. The temporary table is
empty for a statement with no owning temporary. The dataflow is compile-time.
There are no drop flags, so there are no runtime branches. The parity programs
hold only numbers and stay exactly where they are.

The one cost is real and worth stating: a release that runs at statement end
rather than never is work the program did not do before. That is the point, and
the measurement to keep beside it is the one in the table above — 313.9 MB
against 8.5 MB is not a tradeoff, it is a defect.

## What this RFC does not decide

Whether a temporary should be released at the end of the STATEMENT or at the end
of the enclosing expression. The statement is simpler and is what a C compiler
does with a temporary's lifetime; the expression is tighter and needs the release
threaded through evaluation order, which is where the PR #61 sha1 lesson —
release AFTER the new value is built — was learned the first time.
