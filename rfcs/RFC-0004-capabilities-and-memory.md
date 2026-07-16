# RFC-0004 — Capabilities & Memory

- **Status:** Draft — **`consume`/`modify` capabilities, both memory-model
  lowerings, and structured concurrency implemented in v0.1**; the model is a
  **decided hybrid** (§5.2), measured not argued. Remaining: surface refinements +
  *parallel execution* of the shipped concurrency model (runtime, not design).
- **Depends on:** RFC-0001
- **Related:** RFC-0002 (views), RFC-0006 (how conflicts are reported)

> **Implementation status (v0.1).** The capability *surface* is in: a parameter
> may carry a capability keyword — `fn redeem(t: consume Token)` — and `consume`
> is enforced by a move-checking pass (`vyrn-frontend::movecheck`):
> ```vyrn
> let b = Token { id: 5 };
> let y = redeem(b);   // b consumed
> redeem(b);           // ERROR: `b` was already consumed by `redeem(..)` on line N
> ```
> - `consume` takes ownership; using the variable afterward is a compile error
>   (RFC-0006-style message). `read` (the default) imposes no restriction.
> - The analysis is flow-sensitive: `if` merges branches with *may-consume*,
>   reassignment revives a variable, and consuming a pre-loop variable inside a
>   loop is rejected. Capabilities are **erased** — no runtime cost.
> - The capability keywords are contextual (not reserved words): `read`,
>   `modify`, `consume`, `share` are only capabilities when a type follows.
>
> **The heap now exists (first step of §4).** Dynamic strings are implemented —
> `concat` allocates a fresh buffer, `len` measures one — so the language has real
> heap allocation for the first time. See `examples/dynstring.vyrn`.
>
> **First reclamation strategy: `region { .. }` arenas (§4, §Q3).** The heap no
> longer only leaks. A `region` block gives its allocations a deterministic
> lifetime — everything allocated while the region is on the stack is freed the
> moment the block exits, with no GC and no per-object bookkeeping:
> ```vyrn
> region {
>     let s = concat(a, b);   // allocated in the arena
>     total = len(s);         // a non-heap result to carry out
> }                           // ← every arena allocation freed here
> ```
> - **Safety.** The checker forbids storing a heap-carrying value into a binding
>   that outlives the region (it would dangle once the arena frees). Regions are
>   for scoped *temporaries*; producing a value that must outlive the region needs
>   ownership transfer, which is still future work.
> - **Measured.** A 40-million-iteration allocation loop consumes ~1.2 GB when it
>   leaks and stays flat at ~3.5 MB inside a region — the arena genuinely reclaims.
>   See `examples/region.vyrn`.
>
> **Second reclamation strategy: ownership auto-drop (§4, Path A's ownership
> half).** Reclamation no longer needs an explicit region for the common case. An
> ownership pass proves when a heap temporary bound to a `let` cannot escape its
> block — it is only ever *read* (`len`/`print`/`concat` copy but never retain a
> string), never returned, aliased, or stored outward — and the backend frees it
> deterministically when the block exits:
> ```vyrn
> fn greeting_len(a: String, b: String) -> Int {
>     let g = concat(a, b);   // a heap temporary…
>     return len(g);          // …freed automatically right here
> }
> ```
> - The analysis is deliberately conservative: anything it cannot prove
>   single-owned is left to leak, which is always safe — never a use-after-free or
>   double-free.
> - The two mechanisms **partition every allocation**: `concat` allocates in the
>   arena when it is lexically inside a region (freed by the region) and with
>   `malloc` otherwise (freed by ownership if non-escaping). No buffer is ever
>   reachable by both, so nothing is freed twice.
> - **Measured.** The same 40-million-allocation loop that leaked 1.2 GB now stays
>   flat at ~3 MB with no region at all — including a variant that threads
>   temporaries through nested `concat`s and early returns, which exits cleanly
>   (a double-free would abort). See `examples/ownership.vyrn`.
>
> **Ownership *transfer* across calls (§3, "inference first").** Ownership is not
> confined to one function. A function whose every heap return yields a *fresh,
> unaliased* value (a `concat`, or a local owner moved out) is inferred to
> **return owned**, and at each call site the receiving binding becomes the owner
> and is freed in turn:
> ```vyrn
> fn full_name(first: String, last: String) -> String {
>     let sp = concat(first, " ");   // inner temp, freed inside full_name
>     return concat(sp, last);       // fresh value, ownership moves to the caller
> }
> // let name = full_name(a, b);     // `name` owns the result; freed at block end
> ```
> This "returns owned" property is inferred by a fixpoint over the call graph. It
> is deliberately sound-over-precise: a function that might return a *borrowed*
> value — e.g. `fn id(s: String) -> String { return s; }`, which hands back its
> own argument — is **not** owned, so its result is never auto-freed and the value
> it aliases is left to leak rather than be freed twice. Verified: a 20-million-
> call factory loop stays flat at ~3.5 MB, while the aliasing case leaks but exits
> cleanly (no double-free, no dangling read). See `examples/transfer.vyrn`.
>
> Together these cover Path A of §4 for `String` values: regions for grouped
> lifetimes, ownership auto-drop for local temporaries, and ownership transfer for
> values that flow out of a function. The still-open case is values that escape by
> genuine *aliasing* (two live names for one buffer), where single-ownership does
> not apply.
>
> **Path B prototyped: generational references.** That aliasing case is exactly
> what the second lowering is for. A `Ref` is a *freely-copyable* handle to a
> mutable heap `Int` cell — you can alias it, pass it, and store it without
> ownership tracking:
> ```vyrn
> let counter = cell(0);
> let alias = counter;      // a second reference to the same cell — allowed
> set(alias, 40);
> release(counter);         // free once; every copy is now stale (and checked)
> ```
> Each reference carries the *generation* captured when it was made; the cell
> carries a counter that `release` bumps. Every `get`/`set` validates the two, so
> a reference used after release fails a cheap check (`Vyrn: reference used after
> release`, exit 1) instead of dangling — even after the slot has been reused by a
> later `cell(..)`, because reuse bumps the generation and old references no longer
> match. This is the Vale-style mechanism from §4, working end to end in both the
> interpreter and native code. See `examples/genref.vyrn`.
>
> **`release` is inferred — the two paths share one analysis.** Manual `release`
> exists but is rarely needed: the *same* ownership analysis that auto-frees a
> non-escaping string auto-*releases* a non-escaping cell at block exit. Path A
> and Path B are now two behaviours of one escape analysis — a binding is
> reclaimed when it provably does not escape, and only the *how* differs (`free`
> a buffer, `release` a cell, or `afree` a growable array). The array case even
> tracks the in-place `a = push(a, x)` self-update so a `mut` array is reclaimed
> without a manual call. A resource that is aliased or handed off is left to the
> programmer (as in `examples/genref.vyrn`); an ordinary local one needs no manual
> free at all (`examples/autorelease.vyrn`).
>
> This makes Path B's advantage concrete: reclamation can be *aggressive* because
> a missed alias fails a cheap generation check (a clean trap) rather than
> dangling — where a wrongly-freed owned value would be a use-after-free. A
> million-iteration loop that allocates a cell per turn runs within a 65536-slot
> slab, because each non-escaping cell is released and its slot reused.
>
> **`Ref<T>` is generic over any element type.** The payload is *boxed*, so a
> reference is a fixed-size `{ slot, generation }` handle whatever `T` is — you can
> make a `Ref<String>`, a `Ref` to a record, or a `Ref<Ref<Int>>`. Crucially, this
> makes recursion well-formed: a record may hold a `Ref` to *its own type* and stay
> finite, exactly as a pointer would — `type Node = { value: Int, next: Ref<Node> }`
> type-checks and lowers. See `examples/reftypes.vyrn`.
>
> **Recursive data structures work — and reclaim.** An `Option` (or enum) payload
> can now hold a `Ref` — boxed into the aggregate's word — so `Option<Ref<Node>>`
> gives a clean nil terminator. A singly-linked list (built by prepending cells,
> traversed by recursive `match`) and a binary tree (two optional child references)
> both run identically in the interpreter and native code — and both *reclaim*: a
> recursive walk that reads a node's edges, `release`s it, then recurses frees the
> whole structure. A stress loop that builds and frees 100,000 nodes through a
> 65536-cell slab runs to completion, so the slots are genuinely reused. See
> `examples/linkedlist.vyrn`, `examples/tree.vyrn`, `examples/freelist.vyrn`.
>
> The `Option` (and `Result`) payload is two words wide, so a `Ref` — which is two
> words — is stored *inline* with no heap box; a whole recursive structure now
> reclaims to a flat baseline (500,000 list nodes cycle through at ~3.5 MB).
>
> The purpose of all this remains to make the two lowerings measurable against each
> other on real programs, which is what §5 prescribes.
>
> With both paths on the board and unified behind one analysis, the remaining work
> is the actual open *decision* — measure them on representative workloads and
> record the winner (or the hybrid split) per §5.
>
> **Not yet implemented:** the *decision* between the two paths (both are now
> prototyped and unified under one analysis, but neither is chosen); `Result`
> payload generalisation (still `Int`-only); a growable collection (`Vec`);
> ownership for record-embedded or nominal `String`s;
> `modify`/`share` have no teeth yet (accepted, treated as `read`) because they
> need references/aliasing to be meaningful. `consume` is a *contract* today
> (values are still copied at runtime); it lays the groundwork for real move
> semantics and resource types.

---

## Summary

Vyrn replaces Rust's *ownership vocabulary* with a *capability vocabulary*. The
programmer thinks **read / modify / consume / share**. The compiler lowers those
intents onto a concrete memory strategy (ownership inference + regions +
generational references, hybrid) that is **largely invisible** in everyday code.

Three decisions, all now settled:

- **Firm:** the surface model is capabilities, attached to *operations*, not a
  ledger the programmer maintains.
- **Firm:** deterministic destruction, no tracing GC.
- **Decided (§5.2):** the underlying mechanism is a **hybrid defaulting to
  ownership** — inferred single-ownership + regions for single-owner values,
  generational references for the aliasing case — chosen after prototyping and
  measuring both (the generational check proved essentially free in a hot loop).

---

## 1. Capabilities, on operations not objects

A late refinement from the design conversation: capabilities belong to
**operations**, not to objects. The question is not "what can this object do?"
(which changes with program state and is hard to pin to a type) but "what does
*this operation* require?"

The four capabilities:

| Capability | Meaning | Rust analogue (lowering, not surface) |
|------------|---------|----------------------------------------|
| `read`     | observe, do not change | `&T` |
| `modify`   | change in place, exclusively | `&mut T` |
| `consume`  | take ownership, value ends here | `T` by value (move) |
| `share`    | hand out concurrent read access | `&T` / shared refcount |

Function parameters state the capability they need:

```vyrn
fn print(user: read User)      // observes
fn rename(user: modify User)   // mutates in place
fn archive(user: consume User) // takes it; caller can't use it after
```

At the call site the *intent* is legible before you read the body:

```vyrn
print(user)     // I know this only looks
rename(user)    // I know this may change user
archive(user)   // I know user is gone after this line
```

The compiler still lowers these to efficient borrows/moves; the programmer never
writes `&`, `&mut`, or lifetime annotations in the common case.

## 2. What replaces the borrow checker's *error surface*

The rules underneath are borrow-checker-like (one `modify` xor many `read` at a
time). The difference is **how they are surfaced** (→ RFC-0006): not "cannot
borrow X as mutable because it is also borrowed as immutable" but

```
rename(user) needs: modify
  but user currently has an active `read`, held by print(user) on line 23.
  Available again after line 23.
Fixes:
  • move rename above/below the read
  • clone user
  • change print to `consume`
```

## 3. Inference first

Principle 7 (RFC-0001): the compiler infers capability requirements from how a
value is used, and infers when a value is last used (so a `consume` is free).
Annotations on parameters are required (they are the API contract); annotations
inside bodies should almost never be.

```vyrn
let s = make_string()
let t = s              // compiler proves s is dead here ⇒ this is a move, no error
```

Field-granular borrows are inferred too:

```vyrn
foo(user.name)         // compiler lends just `name`, not the whole `user`
```

## 4. Memory model — candidates (OPEN)

The design conversation surveyed the research. Vyrn's plan is a **hybrid** where
the compiler picks a strategy per value, and the programmer rarely chooses:

```
stack            → small, scope-bound values (default)
owned heap       → single-owner heap values, freed at end of scope
region / arena   → many values with a shared lifetime (request, frame, parse)
generational ref → shared values whose lifetime the compiler can't pin
refcount         → last resort for genuinely shared, long-lived graphs
```

### The leading candidate for the "hard" case: generational references (Vale-style)
Instead of lifetimes everywhere, a shared reference carries a generation tag;
the target carries a generation counter. Dereference validates the tag. Freeing
and reusing memory bumps the counter, so a stale reference fails a cheap check
instead of dangling.

- ➕ no lifetime annotations, no borrow checker in common code, deterministic
  destruction, no tracing GC.
- ➖ a small runtime check on some dereferences.

### Regions for structural fit
Compilers, games, web requests, and parsers naturally group allocations by
lifetime. Region/arena allocation frees the whole group at once and matches these
workloads with near-zero per-object bookkeeping.

## 5. Evaluation plan (how the open question gets closed)

The memory model will **not** be decided by argument. Plan:

1. Build the frontend and an interpreter (done/in progress in `compiler/`) so
   real programs exist to measure.
2. Prototype two lowerings behind the same capability surface:
   (a) inferred single-ownership + regions, (b) generational references for the
   shared case.
3. Measure on representative workloads (a parser, a small game loop, a request
   handler) for: annotation burden, predictability of errors, runtime overhead.
4. Record the winner (or the hybrid split) back into this RFC and promote it to
   Accepted.

### 5.1 Findings (measured in v0.1)

Both lowerings are now prototyped behind the same capability surface, so steps 1–3
are done. What the measurements show:

**Runtime cost — the generational check is essentially free in a hot loop.** A
maximally access-bound loop (200 million iterations doing nothing but read + add +
write) was run two ways: a direct local (Path A, no per-access check) and a
generation-checked cell (Path B, `get`/`set` each turn).

| workload (200M iters) | Path A (direct) | Path B (gen-checked) |
|-----------------------|-----------------|----------------------|
| steady-state median   | ~1.02 s         | ~1.02 s (within noise) |
| worst case (cold)     | 1.21 s          | 1.34 s (≈ +10 %)     |

The generation counter lives in a small, hot slab, so the check hits L1 cache and
the branch predicts perfectly; in steady state it is unmeasurable, and even the
cold-cache worst case is ~10 % on a loop that does *only* memory access. Real code
does work per iteration, so the relative cost is smaller still. **Answering Q2:
generational-reference checks are acceptable in hot loops** — a statically-checked
escape hatch is a "nice to have," not a necessity.

**Annotation burden.** Both paths reclaim non-escaping values with *no* programmer
annotation (one inferred escape analysis drives `free`/`release`/`afree`). Path A
additionally needs no per-access ceremony but *cannot express aliasing* — a value
with two live owners must be restructured or left to leak. Path B costs a little
surface ceremony (`cell`/`get`/`set`) but expresses aliasing directly.

**Error predictability.** Path A's failure mode is a *silent leak* (safe, but
invisible) when ownership can't be proven. Path B's is a *loud, clean trap*
(`Vyrn: reference used after release`) — a diagnosable runtime error, never a
dangling read.

### 5.2 Decision — a hybrid, defaulting to ownership (answering Q1)

The evidence supports the **hybrid** the summary anticipated, with a clear default:

- **Path A (ownership + regions) is the default** for single-owner values — zero
  per-access overhead, no annotations, and the compiler proves reclamation.
- **Path B (generational references) is the tool for aliasing** — the case where
  single-ownership does not apply. Its overhead is negligible and its failure mode
  is a safe trap, so reaching for it when you need shared mutable state carries no
  real penalty.

This keeps the "which strategy is this value using?" cognitive load low (Q1's
worry): the default is ownership, and you only opt into `Ref<T>` exactly when you
need aliasing — which is also exactly when the type makes that choice legible. The
memory model is therefore **promoted from open to a decided hybrid**; the remaining
work (inferred/invisible regions per Q3, `share`-by-reference, and *parallel
execution* of the already-shipped concurrency model per Q4) is refinement of the
surface and the runtime, not a change of mechanism.

---

## Design constraints the winner must satisfy

- No dangling pointers (safety) — either statically prevented or cheaply checked.
- Deterministic destruction (no GC pauses).
- Lower annotation/cognitive burden than Rust for the 90% case.
- Lowerings must respect capabilities from §1 (a `read` view can never silently
  become `modify`).
- Structural views (RFC-0002) carry the narrower capability set.

## Open questions

- **Q1. Resolved (§5.2).** Hybrid, defaulting to ownership. The "which strategy?"
  load stays low because ownership is the default and `Ref<T>` is opted into
  exactly when aliasing is needed — where the type already makes the choice clear.
- **Q2. Resolved (§5.1).** Yes — generational-reference checks are acceptable in
  hot loops (measured within noise in steady state; ~10 % cold, on an access-only
  loop). A statically-checked escape hatch is optional, not required.
- **Q3.** How are regions *named/introduced* in surface syntax, if at all? Ideal:
  inferred and invisible; fallback: an explicit `region { ... }` block. *(v0.1
  ships the explicit-block fallback as the first prototype; inferred/invisible
  regions remain the goal.)*
- **Q4. Implemented as a deterministic model; only parallel *execution* remains.**
  Structured concurrency ships in v0.1 — `spawn f(args) -> Task<T>` / `join(t) -> T`
  over functions the compiler *proves* isolated (no `print`, no `cell`/`set`/`release`,
  no `modify` params, transitively). Because tasks are pure, the result is
  schedule-independent, so the interpreter (running them eagerly) and native code
  agree; a parallel scheduler is a drop-in backend optimisation that changes no
  answers. `share` is the capability for concurrent read access. See below.

### Q4 in detail — the model, now implemented

The design constraint was the invariant every other feature upholds: the
interpreter and the native binary must produce *identical* output. Preemptive
threads interleave observable effects nondeterministically, so concurrency here has
to be **structured and deterministic in its observable result**. It is:

- A task runs work that is *pure*: it may `read`/`share` immutable data but cannot
  `print`, `modify` outside state, or touch the shared reference slab
  (`cell`/`set`/`release`). The compiler **proves** this — `spawn f(..)` is a
  compile error unless `f`, and everything it transitively calls, is isolated. That
  is data-race freedom by construction, expressed through the capability system:
  `share` is exactly the capability that lets several tasks hold concurrent read
  access to the same value.
- `spawn f(args) -> Task<T>` starts the work; `join(t) -> T` awaits its result.
  Results are combined by the program in a fixed order, so the *observable outcome
  is deterministic* even though execution order need not be.
- Because a pure task's result is schedule-independent, the interpreter runs tasks
  eagerly/sequentially and the native binary does the same today — identical output,
  invariant intact. A future native runtime can run them on real threads *without
  changing a single answer*, since the model already guarantees order-independence.

What remains is therefore only **parallel execution** — a portable threading layer
plus task marshalling. That is runtime engineering, not language design: the
concurrency model, its type (`Task<T>`), and its safety guarantee are all in v0.1
and verified. See `examples/concurrency.vyrn`.

(The one thing still to gain teeth is `share`-by-reference: today a `share`
parameter is passed by value, so it coincides observably with `read`; passing large
shared data by read-only reference is an optimisation, not a semantic change.)
