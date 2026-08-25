# The release algorithm, and what it proves

Appendix A to [RFC-0114](../RFC-0114-ownership-at-a-program-point.md).

A formal statement of the insertion algorithm, the invariant it maintains, and
three theorems: **no double free**, **no leak**, **no use-after-free**.

The point of writing it out is not ceremony. Three defects were fixed in this
area in one day, and every one of them was a violated assumption rather than a
broken proof step. Section 8 names which assumption each violated, and Section 9
says plainly what is NOT proven.

---

## 1. Scope

The algorithm decides **where a compiled backend emits a release**. It does not
decide what a release DOES for a given type — that is `Owned::release_kind`, a
separate table — and it does not decide which values are owned in the first
place, which is the move checker.

It is stated for one function body at a time. Cross-function reasoning enters
only through the classification of calls (§4), which is a parameter of the proof.

---

## 2. The model

### 2.1 Places

A **place** is anything a release can name:

- `Vars` — the `let` bindings and parameters of the function.
- `Temps` — the results of subexpressions that no binding names. Each syntactic
  expression node that produces a value is a distinct temp.

Write `P = Vars ∪ Temps`.

### 2.2 Static state

The analysis assigns each place, at each program point, an element of

```
S  =  { ⊥ , O , B , M , ⊤ }
```

| | meaning |
| --- | --- |
| `⊥` | not yet initialised; holds nothing |
| `O` | **owns** an allocation: this frame is responsible for releasing it |
| `B` | **borrows** an allocation someone else owns; must never release it |
| `M` | **moved**: held an allocation, gave it away; must never release or read it |
| `⊤` | the analysis cannot say — see §5.3 |

Ordered by `⊥ ⊑ x ⊑ ⊤` for every `x ∈ {O, B, M}`, with `O`, `B`, `M` pairwise
incomparable. `S` is a lattice of height 2. Join is least upper bound, so any
disagreement between two incoming edges is `⊤`.

A **static state** is `σ : P → S`. Write `σ[p ↦ s]` for update.

### 2.3 Dynamic state

A **runtime state** is `(H, ρ)` where `H` is the set of live heap allocations and
`ρ : P ⇀ H` says which allocation each place currently holds. `ρ(p)` undefined
means the place holds nothing releasable.

`free(a)` removes `a` from `H`. Freeing `a ∉ H` is a **double free**. Reading
`ρ(p) = a` with `a ∉ H` is a **use-after-free**. An allocation in `H` when the
frame ends and reachable from no live place is a **leak**.

---

## 3. Assumptions

These are properties of the LANGUAGE, checked elsewhere. The theorems are
conditional on them, and §8 shows what happens when one is false.

- **A1 (no read after move).** If `σ(p) = M` at a point, the program does not
  read `p` there. *Enforced by the move checker; this is RFC-0089 rule 1.*
- **A2 (a borrow does not escape).** A value in state `B` is never stored into a
  place, returned, or captured. *RFC-0089 rules 2 and 3.*
- **A3 (a return is owned).** A function's result is an allocation the caller
  owns, never a borrow of the callee's storage. *RFC-0089 rule 3; `movecheck`
  refuses the program where it is false.*
- **A4 (classification is total and correct).** Every primitive and every call
  position is classified by §4, and the classification is true of the
  implementation.
- **A5 (no unwinding).** A trap ends the process. There is no path on which a
  release is skipped by an exception. *Stated by RFC-0079.*

---

## 4. Classification

Each operation that produces a value is exactly one of:

| class | meaning | new owner? |
| --- | --- | --- |
| `Alloc` | returns a fresh allocation | yes — the result is `O` |
| `Lend` | returns storage it was given, or a pointer into it | no — the result is `B` |
| `Move` | takes an argument's ownership | the argument becomes `M` |
| `Pure` | returns nothing releasable | no |

Each **argument position** of a call is exactly one of:

| verdict | the callee | the caller |
| --- | --- | --- |
| `Released` | borrows and keeps nothing | still owns; releases after the call |
| `Transferred` | takes ownership | the argument becomes `M` |
| `Retained` | keeps it beyond the call | the caller must not release |
| `Lent` | hands part of it back in the result | the caller must not release |

In the implementation these are `own::str_temporary` / `Owned::owned_fns` and
`movecheck::ArgVerdict`. **A4 says this table is right.** It is the entire
surface on which the proof can fail, which is §8's subject.

---

## 5. The algorithm

### 5.1 Transfer

For a statement `s` and incoming `σ`, `⟦s⟧(σ)` is:

```
⟦let x = e⟧ σ      =  σ' [x ↦ cls(e)]           where σ' = ⟦e⟧ σ
⟦x = e⟧ σ          =  σ' [x ↦ cls(e)]           where σ' = ⟦e⟧ σ
⟦drop x⟧ σ         =  σ  [x ↦ M]
⟦return e⟧ σ       =  σ' [e ↦ M]                where σ' = ⟦e⟧ σ
⟦f(e₁ … eₙ)⟧ σ     =  σₙ with, for each i:
                        verdict(f, i) = Transferred  ⟹  σ[eᵢ ↦ M]
                        otherwise                    ⟹  σ unchanged
```

with `cls(e) = O` if `e` is `Alloc`, `B` if `Lend`, and undefined (untracked) if
`Pure`.

### 5.2 Release points

`R(π)` — the set of places released at point `π` — is defined by exactly four
rules, and by no others:

```
R1  after a call f(… eᵢ …):   { eᵢ | verdict(f,i) = Released ∧ σ(eᵢ) = O }
R2  at a store  x = e:        { x }  if σ_before(x) = O ∧ cls(e) ≠ Lend-of-x
R3  at  drop x:               { x }  if σ_before(x) = O
R4  at every exit of the body: { p ∈ Vars | σ(p) = O }
```

R2's side condition is the `@push(a, i)` case: an operation whose result is the
same allocation the place already holds is `Lend`, not `Alloc`, so the store must
not release.

Every release sets `σ(p) := M` immediately after.

### 5.3 The join, and why `⊤` is refused

At a control-flow merge, `σ = ⊔ σᵢ` pointwise. If `σ(p) = ⊤` for a place that is
live after the merge, the program is **rejected** with a diagnostic naming `p`.

The alternative is a **drop flag**: a runtime boolean recording which branch ran,
tested before the release. This algorithm refuses it, for a reason that is not
taste:

> **`⊤` never occurring is what makes `σ` EXACT rather than an approximation, and
> exactness is what Theorems 1 and 2 need.** With a drop flag, `σ(p) = ⊤` means
> "maybe", every theorem becomes a statement about the flag rather than about the
> program, and the proof obligation moves into generated code that must itself be
> shown correct on every path.

Refusing `⊤` keeps the whole argument static.

> **SUPERSEDED for silent releases — see Part II.** Refusal is not the optimum.
> §12's edge normalization accepts every program the move checker accepts,
> keeps the analysis exact, and adds no runtime state; `⊤` becomes UNREACHABLE
> rather than refused (Theorem 4). Refusal survives only for the one case
> normalization cannot serve, §17's observable conditional move, and Part II
> says exactly why.

### 5.4 Fixpoint

`S` has height 2 and the transfer functions are monotone, so the standard forward
dataflow over the CFG terminates in at most `2·|P|` iterations per function. On a
reducible CFG with a loop, the loop head's state is the join of the entry edge and
the back edge; a place assigned inside the loop and consumed after it reaches `⊤`
at the head and is rejected by §5.3 — which is exactly the "`key` is consumed by
`m` inside a loop" diagnostic the language already emits.

---

## 6. The invariant

**I(π)** holds at program point `π` when, for the static `σ` at `π` and the
runtime `(H, ρ)`:

- **I₁ (owners are live).** `σ(p) = O ⟹ ρ(p) ∈ H`.
- **I₂ (owners are unique).** `σ(p) = O ∧ σ(q) = O ∧ p ≠ q ⟹ ρ(p) ≠ ρ(q)`.
- **I₃ (nothing else is a responsibility).** `σ(p) ∈ {⊥, B, M} ⟹` this frame does
  not release `p`.
- **I₄ (coverage).** Every `a ∈ H` allocated by this frame and not yet
  transferred out satisfies `∃p. σ(p) = O ∧ ρ(p) = a`.

**Lemma 1.** `I` holds at function entry.
*Proof.* Parameters: a `consume` parameter is `O` and holds a distinct
allocation, since the caller moved it and by I₂ at the caller no two arguments
name one allocation; a `read` parameter is `B`. Locals are `⊥`. The frame has
allocated nothing, so I₄ is vacuous. ∎

**Lemma 2.** Each transfer of §5.1, together with the releases of §5.2,
preserves `I`.

*Proof.* By cases.

- **`Alloc` into `x`.** A fresh `a ∉ H` enters `H` and `ρ(x) = a`; `σ(x) := O`.
  I₁ holds. I₂ holds because `a` is fresh, so no other place holds it. I₄ holds
  because `x` covers `a`. If `x` previously held `O`, R2 released it first and
  set it to `M` — so no place is left claiming the old allocation, and by I₄'s
  previous instance nothing else did.
- **`Lend` into `x`.** `σ(x) := B`. `H` is unchanged. I₂ is not threatened
  because `B` places are not owners. I₄ unchanged: the allocation still has its
  original owner.
- **`Move` (an argument at `Transferred`, a `return`, a store).** `σ(e) := M`.
  The allocation leaves the frame's responsibility, so it leaves I₄'s scope; the
  receiving frame owns it, by the same invariant at the callee's entry
  (Lemma 1).
- **A call with a `Released` argument.** The callee is given `ρ(eᵢ)` as a `B`
  parameter. By **A2** it stores nothing, so on return no place outside the
  caller names `ρ(eᵢ)`, and by I₂ within the caller only `eᵢ` does. R1 releases
  it and sets `M`. I₁–I₄ hold after.
- **A call with a `Lent` argument.** No release. The result is `B`. I₄ still
  attributes the allocation to the original owner. ∎

---

## 7. The theorems

### Theorem 1 — no double free

*Every allocation is freed at most once.*

*Proof.* Suppose `a` is freed twice. A free happens only through R1–R4, each of
which requires a place `p` with `σ(p) = O` and `ρ(p) = a`, and each of which sets
`σ(p) := M` immediately.

For a second free, some place `q` must have `σ(q) = O ∧ ρ(q) = a` at a later
point. Two cases.

1. `q = p`. Then `σ(p)` returned to `O` after being `M`, which happens only by a
   store into `p` (§5.1). By **A1** the stored value cannot have been read from
   `p`, and by I₄ after the first free `a` is owned by no place, so nothing in
   the frame can produce `a` to store. The store therefore wrote some `a′ ≠ a`.
2. `q ≠ p`. Then at the first free, both `p` and `q` had `O` and held `a`,
   contradicting **I₂**.

Both cases are impossible, so no second free exists. ∎

### Theorem 2 — no leak

*Every allocation created by the frame and not transferred out is freed exactly
once.*

*Proof.* Let `a` be such an allocation, still in `H` at the frame's exit point.
By **I₄** there is `p` with `σ(p) = O` and `ρ(p) = a` at that point. If
`p ∈ Vars`, R4 releases it. If `p ∈ Temps`, then `p` was the argument of some
call; its verdict is not `Transferred` (else `a` left the frame) and not
`Retained` or `Lent` (else `a` is not the frame's by I₄), so it is `Released` and
R1 released it at the call — contradiction with `a` still being live. Hence `a`
is freed, and by Theorem 1 exactly once.

**A5** is what makes "the exit point" total: with no unwinding there is no path
that leaves the body without passing an exit at which R4 runs. ∎

### Theorem 3 — no use-after-free

*No place is read after the allocation it names has been freed.*

*Proof.* A free of `a` through `p` sets `σ(p) := M`. By **A1** the program does
not read `p` while `σ(p) = M`. Any other place `q` with `ρ(q) = a` had
`σ(q) ∈ {B, M}` at the time of the free — `O` is excluded by I₂. A `M` place is
unreadable by A1. A `B` place aliases an allocation owned elsewhere; but the
owner was `p`, and a `B` derived from `p` cannot outlive the release, because by
**A2** it was not stored, so its only uses are within the call that borrowed it,
which returned before R1 ran. ∎

---

## 8. Where the proof actually fails, with three worked examples

Every theorem above is conditional on **A4**: the classification is true of the
implementation. Each defect fixed on 2026-08-25 was an A4 violation, and none of
them was a flaw in §5–§7.

**(a) `owns_heap` on a recursive type.** `type Tree = | Leaf | Node(Tree, Tree)`
was classified `Pure` — "owns nothing" — because the structural walk exhausted a
depth counter and defaulted to `false`. Under the model: an operation that
`Alloc`s was recorded as `Pure`, so I₄ never covered the allocation and R4 had
nothing to release. **Theorem 2's hypothesis was false, not its proof.**
*Symptom: 3.1 GB against a live set of one tree.*

**(b) `mentions_place` at a store.** `s = "x" + s` was classified `Lend` — "the
result is the storage it was given" — because the analysis asked only whether the
new value MENTIONED the place. A concat is `Alloc`: `__vyrn_str_concat` always
calls `__vyrn_str_new`. R2's side condition therefore declined a release it
should have made. **Again A4, and again on the `Alloc`/`Lend` boundary**, which
is where both failures landed. *Symptom: 9.9 GB.*

**(c) The `FreeStr` filter.** This one is NOT an A4 violation and it is worth
separating. The verdict was correct — `Released` — and R1 was correctly derived.
The emitter could not act on it: `arg_frees` held a register with no type and the
free site emitted a String release unconditionally. **The algorithm was right and
the implementation of `free` was partial.** That is a gap in
`Owned::release_kind`'s realisation, which §1 puts outside this proof.

The pattern is worth stating: **the boundary between `Alloc` and `Lend` is where
this proof is load-bearing**, because it is the one place where being wrong in
the safe-looking direction (`Lend`: do not free) is a leak and being wrong the
other way (`Alloc`: free) is a double free. Both of today's classification bugs
chose `Lend` by default. Neither was noticed by a test that compares output.

---

## 9. What is NOT proven

- **A4 itself.** The classification table is asserted, not derived. Every entry
  is a proof obligation discharged by reading an implementation, and §8 is three
  cases where that reading was wrong. A machine-checked version of this appendix
  would start by making `Alloc`/`Lend` a property the emitter's code is checked
  against, not a table beside it.
- **Termination of the release itself.** A declared `impl Owned` release is
  ordinary Vyrn and may recurse. This proof treats `free(a)` as atomic.
- **Cycles.** Two values that reach each other are `Alloc`ed separately and
  owned separately here. A genuine reference cycle in a data structure is not
  representable under I₂ and this algorithm neither creates nor collects one.
- **Concurrency.** `spawn` and `Task` are outside the model; the frame is
  assumed single-threaded, which the spawn-isolation rules already enforce
  separately.
- **That refusing `⊤` accepts every program worth writing.** §5.3 makes the
  analysis exact by rejecting the ambiguous join, and that is a real restriction:
  the program in §10 compiles today. Nothing here counts how many such sites the
  corpus holds. Running the fixpoint over `examples/` and `std/` and reporting the
  `⊤` count is the measurement that turns §5.3 from a preference into a decision,
  and it has not been done.

---

## 10. The `⊤` case, measured

§5.3 refuses a join where a place is moved on one path and not the other. That is
the algorithm's only new refusal, so it is worth knowing what such a program does
TODAY. It compiles:

```vyrn
fn conditional(flag: Bool) -> Int64 {
    let s = build()            // one allocation, ~1000 bytes
    if flag {
        return take(consume s) // moved here
    }
    return s.byteLength        // and not here
}
```

`vyrn check` says `ok`. `vyrn why --memory` says:

```
line 12    s                moved at line 15 into `consume`
```

A per-BINDING summary — one verdict for a place whose state differs by path. On
the branch where the move does not happen, nothing releases `s`. Measured,
200,000 calls with `flag = false`:

| | peak |
| --- | --- |
| the untaken branch | **211.5 MB** |

Which is 200,000 × 1000 bytes, to three figures. Every allocation on that path
leaks.

So the choice in §5.3 is not "refuse a program that works" against "accept it".
It is:

1. **Refuse** — a diagnostic naming `s`, and the programmer restructures or
   `drop`s it explicitly. Static, exact, and what the theorems above need.
2. **Drop flag** — accept, emit a runtime boolean, and move the proof obligation
   into generated code.
3. **What happens now** — accept, summarise the binding as moved, and leak on
   every path where it was not.

Option 3 is the status quo and it is a silent 211.5 MB. That is the argument for
(1) over (3); the argument for (1) over (2) is §5.3's, and it is about where the
proof lives rather than about the program.

**This is a fourth instance of the class in §8**, found while writing this
appendix. It is not an A4 violation — the classification is right — but a case
where the analysis's granularity is per binding and the question is per path,
which is the whole of RFC-0114.

---

# Part II — The complete model

Part I proved the four rules sound and left two things open: the join (§5.3
refused it) and where a temporary dies. Part II closes both, and closes them
OPTIMALLY in a sense made precise in §15: among all schemes that are leak-free
and double-free-free, this one executes the minimum possible number of release
instructions, carries zero bytes of runtime state, and is pointwise-minimal in
heap residency for every silent allocation. The one thing it cannot do is
stated, not smoothed over (§17).

## 11. Two passes

Everything is computed by two classical dataflow passes over the CFG, in this
order.

### 11.1 Liveness (backward)

Standard. `use(n)` = places read at node `n`; `def(n)` = places written.

```
live-out(n) = ⋃ { live-in(m) | m ∈ succ(n) }
live-in(n)  = use(n) ∪ (live-out(n) \ def(n))
```

Bit-vector fixpoint; on a reducible CFG it converges in loop-depth + 2 passes.
"Dead at π" means ∉ live-in(π): not read on any path before being overwritten
or before exit.

### 11.2 Ownership (forward)

The state set SHRINKS from Part I:

```
S  =  { Ø , O , B }
```

`Ø` merges Part I's `⊥` and `M` — "this frame holds no release responsibility
here" — because the release algorithm never needed to distinguish them; only
the move checker does, and it runs first. **`⊤` is not in the set.** Theorem 4
is the licence for removing it.

The classification of §4 gains one class, replacing R2's awkward side
condition:

| class | meaning | transfer |
| --- | --- | --- |
| `Alloc` | fresh allocation | result `O` |
| `Lend` | pointer into storage owned elsewhere | result `B` |
| `Grow(p)` | reads `p`'s allocation, returns THE SAME allocation, possibly reallocated | `p` stays `O`; no release |
| `Pure` | nothing releasable | untracked |

`@push(a, i)` and the in-place append spine are `Grow(a)`. `__vyrn_str_concat`
is `Alloc` — always, which is the fact whose absence was defect (b).

## 12. Edge normalization — the join dissolves

**Rule N.** For every CFG edge `e : n → j` where `j` is a join, and every place
`p` with `σ_e(p) = O` and `p ∉ live-in(j)`:

> emit `release(p)` on the edge `e`, and set `σ_e(p) := Ø`.

That is the entire rule. It is R1–R4's missing sibling — call it **R5** — and
like the others it is a static, unconditional instruction on a specific edge.

**Why it is enough.** Consider any join and any place `p` whose incoming states
differ. The only states are `Ø`, `O`, `B`.

- `O` vs `Ø`, and `p` live after the join: some path from `j` reads `p`, and on
  the `Ø` edge `p` was moved or never initialised — the move checker refuses
  that program (A1 / use-before-init). **This conflict cannot reach codegen.**
- `O` vs `Ø`, and `p` dead after the join: Rule N fires on the `O` edge. Both
  edges now carry `Ø`. **Resolved, and the allocation is released on exactly
  the paths that still held it.**
- `B` vs anything: `B` for a variable arises only from a `read`/`modify`
  parameter, which is not reassignable, so a variable's `B` is path-invariant
  (assumption **A6**, checked by the parser today). A temp's state never meets
  a join at all — a temp's whole life is inside one statement (§13).

**Theorem 4 (closure).** For every program the move checker accepts, the
forward pass with Rule N assigns every place a state in `{Ø, O, B}` at every
point, and every join is between equal states. `⊤` is unreachable.

*Proof.* The case analysis above is exhaustive over `S × S`. ∎

**Corollary (exactness).** `σ(π)(p) = O` implies that on EVERY execution
reaching `π`, `p` holds a live allocation this frame owns. This is Part I's
I₁ made path-universal — and it is the whole performance story, because it is
what lets every release be emitted **without a guard**.

The §10 program is now fixed rather than refused: `s` is `O` on the else edge,
dead at the merge, so R5 releases it there. 211.5 MB becomes one buffer.

## 13. Temporaries, uniformly

A temp `t` is born at its producing node and its liveness is confined to its
statement, so Rule N never sees it. Its death point is fully determined:

| producer / position | death point | rule |
| --- | --- | --- |
| argument at `Released` | immediately after the call returns | R1 |
| argument at `Transferred` | callee's — `σ(t) := Ø` at the call | — |
| argument at `Lent` / `Retained` | not this frame's | — |
| discarded result (expression statement) | end of the statement | R1′ |
| operand of an `Alloc` that copies (concat) | after the consumer | R1″ (this is `own::str_temporary` today) |

R1, R1′, R1″ are one rule — *release at the earliest point past the last
observer* — instantiated at the three positions a temp can occupy. This is
what "statement-scoped temporaries" from the RFC's option table becomes when
made precise.

## 14. Soundness, carried over

Theorems 1–3 hold unchanged, with one addition to Lemma 2's case analysis:

- **Rule N edge.** The released `p` has `σ_e(p) = O`, so by I₁ the allocation is
  live, by I₂ uniquely owned; setting `Ø` restores I₃/I₄ exactly as R3 did. A
  release on an edge is a release like any other; the lemma's cases did not
  depend on WHERE a release sits, only that `σ` said `O` before and `Ø` after.

And the double-free proof gets STRONGER: with `⊤` gone, the exactness corollary
replaces the per-path argument in Theorem 1 case 1 — nothing in the frame can
hold a freed allocation with state `O` on any path, not merely on the analysed
one. ∎

## 15. Optimality — three theorems

**Theorem 5 (release minimality).** On every execution trace, the number of
release instructions executed equals the number of allocations the frame
created (or received by `consume`) and did not transfer out. No leak-free
scheme can execute fewer; this scheme executes no more, and executes **zero
runtime tests** deciding any of them.

*Proof.* Theorems 1+2 give exactly-once per such allocation — that is the
count. Fewer would leave an allocation unfreed (a leak, Theorem 2's
contrapositive). "No tests" is the exactness corollary: every emitted release
sits at a point where `σ = O` holds on all incoming paths, so it needs no
condition. ∎

**Theorem 6 (zero cost off the heap).** A function in which no operation is
classified `Alloc` and no parameter is `consume` compiles to exactly the code
it compiles to today.

*Proof.* All places stay `Ø`/`B` everywhere; R1–R5's guards (`σ = O`) never
hold; the emitted release set is empty. The passes run at compile time only. ∎

**Theorem 7 (pointwise-minimal residency, silent releases).** Among all static,
guard-free placements satisfying Theorems 1–3, placing each SILENT release at
the earliest point where its place is dead on all continuations gives, at every
program point of every trace, a set of live heap allocations that is a subset
of the corresponding set under any other correct placement. Peak heap usage is
therefore minimal.

*Proof.* A correct placement cannot release before last use (Theorem 3 breaks).
The earliest-death placement releases at exactly that frontier. Any other
correct placement releases at the frontier or later on every path, so at every
point its live set contains this one's. Pointwise dominance implies dominance
of the maximum. ∎

## 16. Complexity

| pass | cost |
| --- | --- |
| liveness | O((N + E) · ⌈P/w⌉) per iteration, ≤ loop-depth + 2 iterations, bitsets |
| ownership + Rule N | one reverse-postorder pass; loop heads settle in ≤ 2 visits because Rule N fixes each back-edge state from already-computed liveness |
| emitted code | zero words of runtime state; zero branches; releases = the trace minimum of Theorem 5 |

Both passes are the textbook algorithms; nothing here needs SSA, regions, or an
interprocedural fixpoint — calls enter only through the §4 table, which is
per-signature.

## 17. The one thing normalization cannot do, and the honest split

Rule N moves a release ONTO AN EDGE — earlier than the block exit where the
language's observable semantics puts it. For a **silent** free (a `String` or
`Array` buffer, a payload box) that is invisible and Theorem 7 says it is
optimal.

For a **declared** `impl Owned` release it is not invisible: the release is
user code that may print, and the interpreter — the oracle — runs it at block
exit, keyed on the `let`, from the value the binding took. Moving it to an edge
in the compiled backends would make the three engines run user code at
different times, which is the one thing this project never trades away.

Rust faces the same fork and chooses **drop flags**: a runtime bit so the
scope-end drop knows whether the value is still there. This model refuses that
for the reason §5.3 gave — the memory model is *defined, not inferred*, and a
flag is inference smuggled into the emitted code — and because it breaks
Theorem 5's "zero tests".

So the complete model is a split with a sharp boundary:

| release kind | conditional-move join | placement | cost |
| --- | --- | --- | --- |
| silent (`FreeStr`, `FreeArr`, boxes, `Deep` walks with no user code) | **Rule N: normalized, accepted** | earliest death (Theorem 7) | zero |
| observable (`impl Owned` with a user body) | **refused, with a diagnostic naming the place and both paths** | block exit, matching the interpreter | zero |

The refusal is now a THIN case — only user-bodied releases, only when moved on
one path of a join and live nowhere after — and the diagnostic can say
precisely what to write instead (`drop p` on the non-moving path), which makes
it the same family as "consumed by `m` inside a loop". Everything else, the
211.5 MB case included, is accepted and exact.

## 18. What each open leak becomes under this model

| leak (census) | model verdict |
| --- | --- |
| temporary never released (`check(make(d))`) | R1: `Released` argument, dies after the call. The tree is freed 20,000 times; the wasm measurement (10 MB against 330.5) is this row already running |
| escaping accumulator (`out` consumed at end) | dissolved by per-POINT `σ`: at every store in the loop `σ(out) = O`, so R2 releases the old buffer; the final `consume` sets `Ø` **at the move**, not retroactively for the binding's whole life. The per-binding/per-value confusion cannot be expressed in this model |
| conditional move, silent type (§10) | Rule N releases on the non-moving edge: 211.5 MB → one buffer |
| conditional move, observable type | refused with a diagnostic (§17) — the only refusal the model makes |

## 19. Assumptions, restated for Part II

A1–A5 as in §3, plus:

- **A6 (borrows are path-invariant).** A variable in state `B` is a `read` or
  `modify` parameter and is never reassigned. *Holds today because parameters
  are not assignable; if that ever changes, §12's case analysis gains a real
  `B`-conflict and Theorem 4 needs revisiting — this is the assumption to pin
  with a test.*

And the standing caveat of §9 stands: **the classification table (§4, now with
`Grow`) is the proof's entire attack surface.** Three A4 violations in one day,
all on the `Alloc`/`Lend` boundary, all defaulting to the leaking side. The
model does not make that table right; it makes everything DOWNSTREAM of the
table right, and makes the table small enough to audit — four classes, one
verdict per signature position, and every entry falsifiable by the memory
suite's `Steady`/`Leaks` rows with the interpreter as oracle.

---

# Part III — The unconstrained redesign

Parts I and II answer "is the algorithm right". Part III answers a harder
question: **what is the design under which the last week's defects could not
have been written?** Nothing here is limited to the current code's shape. Three
lenses, because each attacks a different weakness of Parts I–II:

- the **proof-theoretic** lens attacks "the classification table is asserted,
  not derived" (§9's first confession);
- the **systems** lens attacks "a dozen special-case mechanisms, duplicated
  across two backends, each independently wrong-able";
- the **mathematical** lens looks for the theorem that makes the whole design
  inevitable rather than chosen.

They converge, and the point of this part is to show WHERE.

## 20. The master theorem: static ownership is partially evaluated refcounting

The observation that reframes everything: the interpreter is correct with no
analysis because `Rc` makes ownership a property of the RUNTIME, and the
compiled backends are wrong because they make it a property of an analysis that
is not asked. Those are not two designs to reconcile. They are one design at
two stages of evaluation.

**Claim.** The move checker's discipline (A1–A3, A6) is exactly the fragment of
the language on which reference counting is STATICALLY EVALUABLE — every
count's zero-crossing is decidable at compile time.

Make it precise. In the interpreter, passing a value to a `read` parameter
clones the `Rc` (count rises), and the clone dies when the callee returns. By
**A2** a borrow is never stored, so every non-owner handle's lifetime is
strictly nested inside the owner's live range. Therefore:

**Theorem 8 (coincidence).** For a checker-accepted program, every allocation's
strong count reaches zero exactly once, at the point Part II's model marks the
owner `O → Ø` by a release rule — the borrow clones having all died earlier by
nesting. The static release plan and the dynamic zero-crossings are the same
set of events.

*Proof sketch.* Induction over the trace with I₁–I₄. The owner handle is the
`O` place; each borrow clone is created at a call and destroyed at its return,
which precedes any release of the owner on the same path (a release site is
never inside a call that borrowed the value — R1 sits after the call, R2/R3/R5
are statements, R4 is an exit). So the owner's drop is the last handle's drop.
The count's zero point is the owner's release point. ∎

**Theorem 9 (erasure).** A SILENT free — one that runs no user code and writes
no stream — commutes with every observable operation. Hence moving it from the
`Rc`-zero point to the earliest-death point (Theorem 7's placement) preserves
observable behaviour, and only shrinks residency. The one non-commuting free is
a declared `impl Owned` body, which is why §17's split falls where it falls.

*Caveat, stated because it is real:* allocation failure is observable as an OOM
trap. Earlier frees only reduce OOM, so the reorder is sound in the improving
direction; a program that RELIED on running out of memory at a particular
statement was never portable across the three engines anyway.

**Why this matters beyond elegance.** Correctness stops being "the plan
satisfies four rules" and becomes "the plan equals the erasure of the
interpreter" — a statement with a MECHANICAL check (§26). The interpreter is
promoted from test oracle to *definition*.

## 21. Theorem 10: the refusal set is minimal, not preferred

§17 chose to refuse a conditionally-moved `impl Owned` binding. Unconstrained,
one should ask: is that choice forced?

**Theorem 10 (exact characterization).** A checker-accepted function admits a
guard-free, flag-free static release plan that (a) satisfies Theorems 1–3 and
(b) places every observable release at its semantically defined point, **iff**
every place with an observable release kind has path-invariant fate at every
join. Silent kinds never obstruct: Rule N completes their plan on any CFG.

*Proof.* (⇐) is Part II. (⇒): suppose fates differ at a join for observable
place `p` — owned on edge 1, moved on edge 2. Any static plan must decide `p`'s
release downstream of the join. Releasing: doubles on path 2 (Theorem 1
violated). Not releasing: leaks on path 1 (Theorem 2 violated). Releasing on
edge 1: violates (b), the observable point moved. There is no fourth option
without a runtime discriminator — which is the definition of a flag. ∎

So the ONE refused case is exactly the case where no correct plan exists under
the stated semantics. The diagnostic is not the model giving up; it is the
model reporting an impossibility, the way a type error does. That is the
mathematical lens's contribution: **the boundary of the method is
characterized, not chosen** — and anyone proposing to accept that case is
proposing a flag or a semantics change, and now has to say which.

## 22. One data structure, and the deletion list

The systems lens: three leaks in one day, each hidden by a DIFFERENT mechanism
— the depth counter under `owns_heap`, the `mentions_place` guard, the
`FreeStr` filter — and each mechanism duplicated, differently, in `direct.rs`.
The bug factory is not any one guard. It is that **codegen decides**. Two
backends each re-derive ownership facts from scraps (`drop_slots`,
`mentions_place`, filters), so every fact has three implementations that can
disagree: the analysis's, lib.rs's, direct.rs's.

The fix is the boring one: **one table, dumb consumers.**

```
ReleasePlan = [ Obligation { at: PointId, place: PlaceId,
                             kind: DropKind, why: Rule } ]
```

Produced entirely by `own::analyze` + Part II's two passes. Both backends
consume it: at each point, emit the frees the plan names, with the kind the
plan names. A backend makes ZERO ownership decisions.

What that deletes — every one of these is a decision site today, and every one
has hosted or could host a defect:

| mechanism | today | after |
| --- | --- | --- |
| `Gen::slot_owns` / `Fn_::place_owns` | per-binding guess, twice | gone — the plan says |
| `mentions_place` at stores + the `fresh_str` exception | heuristic, twice | gone — `Grow` is a class in the plan |
| `arg_drops` + `FreeStr` filter | verdict discarded by kind | gone — obligations are typed |
| `str_temporary` / `free_str_temp` | String-only special case | an ordinary R1″ obligation |
| the append spine's ownership shadow | its own state machine | a `Grow` obligation stream |
| block-exit `drop_slots` ordering | re-derived per backend | R4 obligations, ordered in the plan |

Six decision mechanisms times two backends collapse into one producer and two
emitters. The prepend bug, the FreeStr bug and the escaping-accumulator bug
become UNWRITABLE — not fixed, unwritable — because the place they were written
no longer exists.

## 23. Wrong frees made unrepresentable

The proof-theoretic lens, applied to the compiler's own Rust. Two mechanisms,
both cheap, both in the spirit of a discipline the codebase already has
(`release_kind`'s "the match has no `_` arm on purpose", own.rs:550).

**Linear discharge.** The plan is consumed through an API that enforces
affinity: `plan.take(at)` yields the obligations for a point at most once, and
`plan.finish()` — called at the end of every function's emission, by both
backends — panics naming any obligation never taken. Skipping an obligation
(the `direct.rs`-forgot-the-fix failure mode that the memory suite caught by
measurement) becomes a loud failure at compile time of the PROGRAM, before any
measurement runs.

**Typed handles.** The wasm emitter's values are `i32` soup, which is why the
textual backend caught the FreeStr misuse (LLVM is typed) and the direct one
would not have. The emitters' handles become a closed enum —
`Handle::Str(reg) | Agg(slot) | Scalar(reg)` — and `emit_free(kind, handle)` is
a TOTAL match with no wildcard. Handing an aggregate to a String free stops
being a runtime corruption and becomes a compiler-compile error; adding a
`DropKind` variant forces every emitter arm to answer.

Together these are the Agda move translated to Rust: not full dependent types,
but the two properties that matter — *every obligation discharged exactly
once*, *no free constructible at the wrong representation* — pushed into types
and asserts that fail the build, not the profile.

## 24. The classification table, witnessed instead of asserted

§9's first confession stands over Parts I–II: `Alloc`/`Lend`/`Grow` is the
entire attack surface, and it was wrong three times in one day, always
defaulting to the leaking side. Unconstrained, the fix is to make every row an
EXECUTABLE CLAIM:

- Each runtime helper and primitive carries its class as data:
  `(name, class, prover)`.
- The **prover** is a test: run the primitive in the interpreter, compare the
  result's allocation identity against every input's (`Rc::ptr_eq`; in the C
  shim's debug mode, pointer equality). `Alloc` asserts freshness against all
  inputs. `Grow(p)` asserts identity with exactly `p` — or a fresh block whose
  old block was freed, the realloc case, observed via the debug allocator's
  log. `Lend` asserts identity with some input.
- A meta-test walks the table: **a row without a prover fails.** A row whose
  prover fails names the primitive and the direction of the lie.

`__vyrn_str_concat` misclassified as `Grow` — defect (b) — dies in that test
the day it is written: the prover observes a fresh pointer and says `Alloc`.
The table remains the axiom set of the proof, but every axiom is now falsified
automatically rather than by a 9.9 GB measurement.

## 25. The oracle, completed: free-trace bisimulation

`memory.rs` compares PEAK at two call counts — it sees leaks. It cannot see a
free at the WRONG POINT that happens to keep peak flat, and it cannot see a
double free at all (that is parity's accidental job). Theorem 8 licenses the
complete check:

- Debug interpreter: log `(allocation-site, birth-index)` at every `Rc`
  zero-crossing.
- Debug compiled builds: the shim and the wasm runtime log every free the plan
  emits, with the same keys.
- The harness asserts the two MULTISETS are equal per program — and, for
  observable releases, that the two SEQUENCES agree.

Equality of multisets is Theorems 1+2 verified per trace. Sequence agreement on
the observable subset is Theorem 9's boundary verified per trace. This is the
bisimulation the coincidence theorem promises, and it subsumes every
`Steady`/`Leaks` row while catching the two failure classes those rows cannot.

## 26. Migration, in the order that keeps every step green

1. **Witness the table** (§24) against today's classifications — this can land
   first and alone, and would already have caught defects (a) and (b).
2. **Build `ReleasePlan`** in `own::analyze` from the facts movecheck already
   computes; assert it AGREES with today's emission on the corpus (plan-vs-IR
   differential) before any backend consumes it.
3. **Switch the textual backend** to the plan behind the linear-discharge API;
   memory suite + parity green.
4. **Switch `direct.rs`**; delete its private copies. `plan.finish()` is what
   makes a missed site loud.
5. **Land Rule N and R1-general** — now one change in one producer, both
   backends inherit it, and the two open `Leaks` rows flip to `Steady`.
6. **Free-trace bisimulation** (§25) as the standing gate; retire nothing —
   peak rows stay as the cheap smoke layer.

Each step is separately revertible and separately testable, which is the
property this week's fixes did not have until the memory suite gained rows.

## 27. What the three lenses agree on

Stated once, because it is the actual conclusion:

> **Move every decision into one artifact whose production is proved (Part II),
> whose consumption is forced (linear discharge, typed handles), whose axioms
> are executable (witnessed classification), and whose ground truth is the
> interpreter (coincidence + bisimulation).**

The proofs in Parts I–II are unchanged by Part III. What changes is where they
attach: to one table produced in one place, instead of to a dozen guards in
three. A proof about a single artifact with forced consumption is worth more
than the same proof about a convention — the last week is the evidence.

---

# Part IV — Foundations

Parts I–III still lean on four informal steps: traces are never defined, the
Galois connection of §20 is named but not constructed, Theorem 8's proof rests
on an unstated bracketing lemma, and no assumption is shown NECESSARY. Part IV
closes each, and ends with the observation that unifies the whole document:
every memory-management strategy is a choice of where to pay for an incomplete
abstraction, and this design's distinguishing property is that it makes the
abstraction COMPLETE.

## 28. The instrumented machine

The language is structured — no goto — so the semantics is structural; Part
II's CFG is a derived view (§37 discharges the correspondence).

A **configuration** is `⟨s, K, ρ, H, β⟩`: the statement under execution, a
continuation stack `K` of call frames, the store `ρ : P ⇀ Addr`, the heap
`H ⊆ Addr`, and the **billing map** `β : H ⇀ P` — the instrumentation. `β` is
not part of the language; it is ghost state that makes ownership a fact of the
concrete semantics so that §30's abstraction has something to abstract.

Steps emit **events**:

```
ev ::= alloc(a)            a fresh, a enters H, β(a) := the receiving place
     | free(a)             a leaves H and dom(β)
     | move(a, p→q)        β(a) := q          (store, consume-arg, return)
     | lend(a, p→q)        β unchanged        (read/modify argument)
     | obs(o)              an output byte, a trap, or ENTRY into a declared release
```

A **trace** is a maximal step sequence from `⟨body, [], ρ₀, H₀, β₀⟩`. Its
**observable projection** `obs(t)` is the subsequence of `obs(·)` events. Two
traces are **observationally equal** iff their projections are equal.

The billing map is total on frame allocations by construction: `alloc` bills
the receiving place, `move` re-bills, `free` un-bills, and nothing else touches
`β`. This replaces Part I's prose "ρ(p) is the allocation p holds" with a
defined object, and I₁–I₄ become statements ABOUT `β`:

```
I₁  σ(p) = O  ⟹  ∃a ∈ H. β(a) = p
I₂  β is injective          (one owner per allocation — now a property, not an axiom)
I₄  every frame allocation is in dom(β) until moved out or freed
```

## 29. The bracketing lemma, and Theorem 8 made whole

**Lemma 3 (lends flow down and close by the next statement).** Every `lend`
event is created by a call expression in the LENDING frame, and its extent ends
when that call returns. Lends therefore form a balanced bracket sequence
against `K` — a Dyck word — and at every statement boundary of a frame, all
lends issued by that frame are closed.

*Proof.* A lend is created only at an argument position with verdict
`Released`/`Lent` (§4) — there is no other constructor of `B`, by A2 no rule
stores one, and by A3 no rule returns one, so a lend cannot travel UP the
stack. Travelling down, the callee may re-lend, and the nesting follows the
call stack. A statement boundary in the lender is reached only after its call
expressions have returned; the brackets opened within the statement are closed
within it. ∎

**Theorem 8, full proof.** Let `a` be a frame allocation with owner chain
`p₀ → p₁ → …` (the `β`-history under moves). Every non-owner handle to `a` in
the interpreter is an `Rc` clone created at a `lend`. The static release of `a`
is one of R1–R5, each of which sits at a statement boundary or immediately
after a call in the OWNING frame — by Lemma 3, every lend of `a` issued by that
frame is closed there, and no other frame can hold a lend of `a` (lends flow
down only from the owner, and the owner is executing). So at the static release
point, the owner's handle is the unique live handle; dropping it is the `Rc`
zero-crossing. Conversely, the interpreter's zero-crossing is the owner's drop,
which the interpreter performs at exactly the release semantics' point for the
binding (block exit / move / drop) — the same point the plan names for
observable kinds, and the erasure-adjusted point for silent kinds (§32). The
two event sets coincide as multisets keyed by allocation, and as sequences on
the observable subset. ∎

## 30. The Galois connection, constructed

Per program point, the concrete domain is `C = 2^States` (sets of reachable
configurations), ordered by inclusion. The abstract domain is `A = P → S` with
`S = {Ø, O, B, ⊤}` ordered pointwise (⊤ re-enters ONLY to state completeness;
Theorem 4 will evict it again).

```
α(X)(p) =  O  if in every state in X, ∃a. β(a) = p and ρ(p) = a
           B  if in every state in X, ρ(p) ∈ H and β(ρ(p)) ≠ p
           Ø  if in every state in X, ρ(p) undefined or ρ(p) ∉ H
           ⊤  otherwise (the states disagree)

γ(σ)     =  { states consistent with σ at every place }
```

`(α, γ)` is a Galois connection by construction (α is the best abstraction of
the three predicates; γ its adjoint).

**Lemma 4 (local soundness).** For every statement kind `s`, the concrete post
`post_s : C → C` and the abstract transfer `⟦s⟧ : A → A` of §5.1 satisfy
`α ∘ post_s ⊑ ⟦s⟧ ∘ α`.

*Proof.* Case by case; each is bookkeeping over the events of §28. `alloc`
bills the receiver, so `α` reads `O` there — matching `cls(e) = O` for `Alloc`.
`move` un-bills the source and bills the target — matching `M`/store transfer.
`lend` leaves `β` fixed — matching `B` with no state change at the owner. The
one non-trivial case is the call with a `Released` argument: by Lemma 3 the
lend closes before the post-state at the following point is formed, so the
argument's abstract `O → Ø` (via R1's release) matches the concrete free. ∎

**Theorem 11 (completeness, the load-bearing one).** With Rule N, on every
checker-accepted program, `α ∘ post_s = ⟦s⟧ ∘ α` — equality, not `⊑` — at every
point; equivalently, the analysis result contains no `⊤` and `γ(σ)` at each
point contains exactly the reachable states' ownership patterns.

*Proof.* Soundness is Lemma 4. For completeness it suffices that no join
introduces `⊤` (transfer functions map non-⊤ to non-⊤ pointwise, by
inspection). That is Theorem 4. ∎

**Why completeness is the whole design.** A sound-but-incomplete abstraction
has states in `γ(σ)` that never occur; any EMISSION driven by `σ` must then be
guarded against them at runtime. That guard is a drop flag. So:

> **A runtime check is the price of the concretization gap.** Drop flags pay it
> per place; reference counts pay it per handle transfer; a tracing collector
> pays it per collection, wholesale. Rule N is none of these because it closes
> the gap itself: it edits the PROGRAM (inserting releases on edges) until the
> abstraction is exact, instead of editing the EMISSION until it tolerates
> inexactness.

This is the single sentence the three lenses of Part III were circling.

## 31. The declarative system Ω, and the algorithm as its elaborator

The dataflow of §5 is an algorithm. The deep object is the TYPE SYSTEM it
implements — stated declaratively, it is a small linear system, and stating it
is what makes the design mechanizable (§37).

Judgments: `Γ ⊢ e : q ⊣ Γ′` (expressions, `q ∈ {O, B, ∅}` the result
qualifier) and `Γ ⊢ s ⊣ Γ′` (statements), with `Γ : P → S`.

```
[ALLOC]   Γ ⊢ e : O ⊣ Γ′                    e classified Alloc, operands checked in Γ, Γ′ their post
[LEND]    Γ ⊢ e : B ⊣ Γ′                    e classified Lend
[VAR-O]   Γ, p:O ⊢ p : B ⊣ Γ, p:O           reading an owned place lends it
[LET]     Γ ⊢ e : O ⊣ Γ′    Γ′, x:O ⊢ s ⊣ Δ, x:Ø
          ─────────────────────────────────────────
          Γ ⊢ let x = e; s ⊣ Δ

[REL]     Γ, p:O ⊢ release p ⊣ Γ, p:Ø       structural; for observable kinds,
                                            admissible only at block-exit position
[IF]      Γ ⊢ e : ∅ ⊣ Γ₀    Γ₀ ⊢ s₁ ⊣ Δ    Γ₀ ⊢ s₂ ⊣ Δ
          ───────────────────────────────────────────────
          Γ ⊢ if e { s₁ } else { s₂ } ⊣ Δ

[WHILE]   Γ ⊢ e : ∅ ⊣ Γ    Γ ⊢ s ⊣ Γ
          ───────────────────────────        the loop invariant IS the context
          Γ ⊢ while e { s } ⊣ Γ
```

The join has vanished: [IF] demands the two branches END in the SAME context,
and [REL] is the structural rule that lets a derivation arrange that — a
release inserted at the end of the branch that still owns is exactly Rule N,
now visible as a use of [REL] before the branch's last inference. [WHILE]'s
invariant context is what the fixpoint computes.

**Theorem 12 (elaboration).** (Soundness) every plan the algorithm produces
corresponds to an Ω-derivation, with the inserted releases as [REL] instances.
(Completeness) if ANY placement of [REL] instances yields an Ω-derivation, the
algorithm finds one — and for silent kinds, the one whose [REL]s are earliest
(Theorem 7). (Boundary) for observable kinds, an Ω-derivation exists iff
Theorem 10's fate-invariance holds; the position side-condition on [REL] is
where the two proofs meet.

*Proof sketch.* Soundness: induction on the CFG walk, mapping each Rule N/R1–R4
emission to [REL]. Completeness: given a derivation, its contexts at each point
form a valid dataflow solution; the least fixpoint refines it, and moving
[REL]s earlier preserves derivability for silent kinds by §32's commutation.
Boundary: [IF] forces equal contexts; without fate-invariance the only
equalizer for an observable place is a [REL] mid-branch, which the
side-condition forbids — the derivation cannot close, matching Theorem 10's
impossibility. ∎

Ω is the artifact to mechanize: intrinsically-scoped syntax, contexts as
functions from a finite place set, and the metatheory (progress + preservation
against §28's machine) is structural induction with no CFG anywhere.

## 32. Erasure, formally

Define an event **silent** iff it is `free(a)` where `a`'s kind runs no user
code — everything except a declared `impl Owned` body.

**Lemma 5 (commutation).** In §28's machine, a silent `free(a)` commutes with
every adjacent event except an `alloc` that fails: no rule reads `H`'s
membership except allocation (for freshness/OOM) and `free` itself (I₂ makes a
second free of `a` unreachable — Theorem 1). `obs` events read `ρ` and values,
never `H`-membership. So exchanging a silent free with a neighbouring non-alloc
event yields a step-for-step equal trace with the same observable projection.

**Theorem 9′ (erasure, full).** Moving every silent free from its Ω-position
([REL] at block exit) to its earliest-death position preserves the observable
projection of every trace, and transforms the heap function `R(τ) = |H(τ)|`
pointwise downward. Against OOM the transformation is an improvement
simulation: every trace that completes before the move completes after it,
because each prefix's heap is a subset. ∎

The interpreter needs no change under this theorem: its silent frees happen at
`Rc`-zero (owner drop), later than the plan's earliest-death point, and §25's
oracle compares MULTISETS for silent frees precisely because Theorem 9′ says
the timing difference is unobservable. Sequences are compared only where
sequences are meaningful — the observable subset.

## 33. Sharpness: every assumption is necessary

Each hypothesis of the theorems has a two-line counterexample without it. This
is what makes the theorems tight rather than merely true.

| dropped | counterexample | first casualty |
| --- | --- | --- |
| A1 (no read after move) | `take(consume s); print(s.byteLength)` | Theorem 3 — the read is a UAF at the freed address |
| A2 (borrows do not escape) | callee stores its `read` argument into module state; owner's R4 fires; the global reads later | Theorem 3, and I₂ across frames |
| A3 (returns are owned) | `fn id(s: read String) -> String { return s }` — the result is billed `O` but aliases the caller's other binding | Theorem 1 — two owners, one buffer, two frees |
| A4 (classification true) | this week, three times, empirically | Theorem 2 (both `Lend`-defaults) — measured at 3.1 GB and 9.9 GB |
| A5 (no unwinding) | an exception path exiting the body between R3 and R4 | Theorem 2 — the exit bypasses R4 |
| A6 (borrows path-invariant) | a reassignable `read` parameter set to an owned value on one branch | Theorem 4 — a genuine `B`/`O` join, `⊤` reachable |

A6's row is the one to guard operationally: it holds today because parameters
are not assignable, and nothing but a parser rule keeps it true. The row IS the
test to write.

## 34. The manager-space theorem

Fix a trace `t` and a frame allocation `a` with birth `b(a)` and last use
`u(a)`. Every sound reclamation strategy frees `a` at some `d(a) ≥ u(a)`.
Define the **offline optimum**: `d*(a) = u(a)⁺`, the point just past last use —
achievable only with full knowledge of the future.

**Theorem 13.** Under the discipline (A1–A6):

```
d_plan(a) = d*(a)                  earliest-death static placement (silent kinds)
d_RC(a)   = owner-drop point       ≥ d*(a)
d_GC(a)   = next collection after unreachability   ≥ d_RC-comparable, unbounded
```

with per-allocation lifetime intervals nested left to right, hence residency
`R_plan ≤ R_RC ≤ R_GC` pointwise on every trace; and the runtime costs are

```
plan: 0        RC: Θ(#handle transfers)        GC: amortized tracing
```

*Proof.* `d_plan = d*` because under A1–A3 the last use is a static fact —
liveness computes it exactly (no aliasing survives the rules to blur it), and
Theorem 7 places the free there. The RC point is the owner's drop, which
follows the last use by Lemma 3. The GC point follows unreachability, which
follows the owner's drop. Nesting gives the pointwise residency chain. ∎

**Reading.** The move discipline's payoff, stated once: it makes the OFFLINE
optimum ONLINE-ACHIEVABLE at zero runtime cost — the gap that refcounting pays
counters for and garbage collection pays pauses for is, in this fragment,
exactly zero. The compiled backends, once on the plan, are not merely as good
as the interpreter oracle; on residency they are strictly better than it, and
§25's multiset (not sequence) comparison for silent frees is what makes the
oracle fair about that.

## 35. Worked derivation: the escaping accumulator

The shape that motivated M2, run through the model. `tag()` is `Alloc`;
`consume` moves.

```
                                        σ(acc)         event / rule
let mut acc = ""                        O   (static "" is Ø-kind: no heap)
loop head, iteration i:                 O ⊔ O = O      [WHILE] invariant holds
    acc = acc + tag()                   O              Alloc; R2 releases the
                                                       PREVIOUS buffer — the
                                                       leak that was 9.9 GB is
                                                       this single row
loop exit:                              O
let b = Held { s: consume acc }         Ø              move(a, acc→b.s)
return b.s.byteLength                   Ø              b: R4 at exit, one free
```

The per-binding view could not say this: it summarized `acc` as "moved", one
verdict for eleven distinct program points with three distinct answers. The
per-point table has no way to even ASK the per-binding question — which is what
"dissolved" meant in §18, now visible line by line.

And the conditional case, with Rule N as a [REL] instance:

```
let s = build()          σ(s) = O
if flag:
    take(consume s)      σ(s) = Ø on this edge
else:
    s.byteLength         σ(s) = O … [REL] fires on this edge (silent kind)
                         σ(s) = Ø
join:                    Ø = Ø        [IF] closes; Theorem 4 witnessed
```

## 36. What Part IV does NOT deepen

- **`β` is ghost state.** The billing map instruments the semantics; it is not
  implemented anywhere. §25's debug logging is its partial realization, and the
  freshness provers of §24 are spot-checks of its `alloc` rule. Full
  realization would be a checked-billing interpreter mode — worth building, not
  yet designed.
- **Ω is stated, not mechanized.** §37 is a roadmap, not a formalization. The
  inductions are structural and the machine is small, which is a claim about
  difficulty, not a proof.
- **The classification table is still the frontier.** Part IV moves it INTO the
  semantics (`Alloc` = the `alloc` event's billing rule) — which makes each row
  a statement about the machine — but the correspondence between the table and
  the actual Rust/C/wasm helpers remains exactly as empirical as §24 left it.

## 37. Mechanization roadmap

In dependency order, each step checkable alone:

1. **The machine** (§28) over structured syntax — configurations, events,
   billing. Small-step, intrinsically scoped. No CFG: the structural [IF]/
   [WHILE] rules make the CFG a lemma, not a definition. (The language is
   goto-free, so the correspondence is an induction over syntax; this is where
   Part II's CFG formulation is discharged.)
2. **Ω** (§31) as an inductive family; contexts as total maps on a finite
   place set.
3. **Preservation + progress** for Ω against the machine, with I₁–I₄ as the
   preserved invariant — Theorems 1–3 fall out as corollaries, replacing Part
   I's prose induction.
4. **Elaboration** (Theorem 12): the dataflow as a function, proved sound and
   complete against Ω. This is the largest proof and the one that certifies
   the IMPLEMENTATION shape, not just the idea.
5. **Erasure** (Theorem 9′) as a trace transformation with a simulation proof.
6. Leave A4 outside the mechanization, permanently: it is the boundary between
   the model and the world, and §24's witnesses are its correct form — tests,
   not theorems.

The order matters because each artifact is USED by the next, and because steps
1–3 alone would already have been worth more than Part I: the three defects of
§8 were all violations of what step 3 preserves.

