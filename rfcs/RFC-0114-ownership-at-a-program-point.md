# RFC-0114 — Ownership At A Program Point

- **Status:** **Draft. The problem is measured; the design is not chosen.**
- **Evidence:** [rfcs/census/declared-release-does-not-run.md](census/declared-release-does-not-run.md).
- **Appendix A:** [the algorithm and its proofs](proofs/release-algorithm.md) — the
  invariant, three theorems, and the assumption each of today's three defects
  violated.

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

So: **refuse the ambiguity.** A place whose state differs across a join is a
diagnostic, not a flag. That keeps the analysis static, keeps the emitted code
free of branches nobody wrote, and gives the programmer the same kind of error
they already get from `consume` inside a loop.

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
