# RFC-0114 — Ownership At A Program Point

- **Status:** **Draft. The problem is measured; the design is not chosen.**
- **Evidence:** [rfcs/census/declared-release-does-not-run.md](census/declared-release-does-not-run.md).

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

## What this RFC does not decide

Whether a temporary should be released at the end of the STATEMENT or at the end
of the enclosing expression. The statement is simpler and is what a C compiler
does with a temporary's lifetime; the expression is tighter and needs the release
threaded through evaluation order, which is where the PR #61 sha1 lesson —
release AFTER the new value is built — was learned the first time.
