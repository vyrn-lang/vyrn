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