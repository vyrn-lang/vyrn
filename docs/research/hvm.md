# HVM, Bend and the interaction-net bet — what Vyrn can learn

Research note. Read on 2026-08-11. Sources and reproduction commands are at the
end.

This note has four parts:

1. What the technology is, without the marketing.
2. What the project set out to prove, and what it delivered.
3. The benchmark record — the claims, the corrections, and the method faults.
4. Takeaways for Vyrn, argued. Angles that do not hold are listed and rejected.

Vyrn is a conventional-execution language. Nothing here proposes that Vyrn adopt
interaction nets. The value is in the failure modes, which are transferable.

---

## 1. The technology

### 1.1 Interaction combinators

Lafont (1990, 1997) defined interaction nets and then a minimal basis for them:
three node types, a small fixed set of local rewrite rules. Two nodes interact
only when their *principal ports* are wired together. Each rewrite touches a
constant number of nodes. The system is **strongly confluent**: if two rewrites
are both available, doing either one first gives the same final net, and the
same total number of rewrites.

Strong confluence is the whole reason the model attracts people who want
parallelism. It means:

- Any worker can take any available rewrite.
- No scheduler decision changes the result.
- No locks are needed for correctness, only for memory safety of the pointer
  updates.

Lafont also proved the model can emulate other models without a complexity
penalty. That is a stronger property than Turing completeness, and it is the
technical basis for Taelin's claim that interaction combinators are the correct
foundation for computing.

### 1.2 Optimal beta reduction, and why duplication decides everything

The lambda calculus has one operation: substitute an argument into a body.
Every real evaluator has to choose how to handle a value that is used twice.

- Call-by-name copies the unevaluated argument and repeats the work.
- Call-by-value or thunk-based evaluation shares the work, but loses sharing
  inside partially applied functions.

Lamping (1990) gave an algorithm where sharing is a first-class node. A `DUP`
node splits a value lazily, and a `SUP` node holds two values in one place. The
algorithm performs the theoretical minimum number of beta steps, and the
theoretical minimum number of clone operations. Some higher-order programs then
run exponentially faster than they do under GHC — Church-encoded arithmetic and
program search over superposed terms are the standard demonstrations.

So duplication is not a detail of the runtime. It is the product. Everything
HVM is faster at, it is faster at because it copied less.

### 1.3 The oracle, and the debt HVM never paid

Lamping's algorithm has two parts. The **abstract algorithm** is the elegant
part. The **oracle** is a set of bookkeeping nodes that make the algorithm
correct on *all* lambda terms. The oracle costs about 10x.

Taelin's engineering decision, carried from Absal through FM-Net into HVM, was
to drop the oracle and accept a restricted input language. The HVM2 paper states
the resulting invariant plainly: a higher-order lambda that duplicates its own
variable may not itself be duplicated. Church exponentiation `2^2` written
directly cannot be reduced soundly.

The paper's remedy is a type system in the source language (elementary affine
logic inference), or a bookkeeping fallback, and it puts both outside the scope
of the work. Bend shipped with neither. That means unsound reduction was
reachable from ordinary user code in the released language. A commenter on the
launch thread raised exactly this; it was never closed.

This is the most important structural fact about the project. The correctness
condition was known, was written down, and was left to a future component that
was never built.

### 1.4 What "automatic parallelism" actually means

It means: every independent rewrite may proceed at once, and the programmer does
not annotate anything.

It does not mean:

- That your algorithm gains parallelism it does not have. Bend's own README
  ships a sequential `sum` and a divide-and-conquer `sum` to show this.
- That the parallelism is at a useful granularity. One interaction is a few
  pointer operations. The runtime pays synchronization at that scale.
- That data locality follows. The unit of work is a graph node, reached by
  pointer.

### 1.5 The cost model, and why it is the ceiling

In HVM2 every operation is a graph rewrite over heap nodes. Taelin stated during
the launch that a numeric operation allocated two nodes. A conventional compiler
puts the same value in a register and allocates nothing.

So the comparison is: a random memory access and an atomic link, against a
register ALU instruction. That gap is the constant factor, and it does not go
away with better codegen. It can be narrowed — native constructors instead of
lambda encodings, tail calls compiled to loops, arrays as native nodes — and the
paper lists all of these as future work. It cannot be removed, because the model
*is* memory traffic.

On the GPU the same property becomes bandwidth pressure plus warp divergence.
Taelin reported during launch week that the compiled CUDA path gave no
substantial speedup over the interpreted CUDA path, and named warp divergence as
the cause. That is a striking admission: on the headline target, compilation did
not pay.

### 1.6 Numbers, effects and evaluation order

The published limitations are severe and are stated honestly in the paper:

- **32-bit ports.** 3 tag bits leave 29 bits of address space: about 500 million
  nodes, about 4 GB. Unboxed numbers are 24-bit integers and 24-bit floats.
- **Eager reduction only.** HVM1 was lazy; HVM2 reduces every available redex.
  A recursive function written the obvious way does not terminate, because the
  recursive branch expands before the condition is decided. The user must split
  the function into several top-level definitions so that references unfold
  lazily.
- **No arrays, no mutation, no IO at launch.** The Bend FAQ listed IO as
  "coming soon", FFI as later, a package manager as later, and named live bugs
  in float conversion and signed integers.
- **Hardware gate.** The CUDA runtime required at least 96 KB of L1 per SM, and
  only the RTX 4090 was tested.

---

## 2. The arc

### 2.1 What Taelin set out to prove

From his own historical account: substitution in the lambda calculus is not
atomic, therefore the standard model is not fundamental; Lamping's graph
rewriting makes it atomic; interaction combinators are the same thing in purer
form; therefore interaction combinators are the correct model of computation and
every existing runtime is carrying avoidable cost.

He tried to prove it four times: Optlam (JavaScript), Absal, FM-Net (C, for
Formality at the Ethereum Foundation), HVM1 (Rust, 2022), HVM2 (2024).

FM-Net failed on speed — Haskell stayed faster. HVM1 reached roughly 30% of GHC
single-threaded, which he read as the first correct architecture, and beat GHC
with threads on some programs, but its parallelism had bugs he could not fix.

### 2.2 What shipped

- **HVM2** (2024): a Rust reference implementation, a C runtime, a CUDA runtime,
  and compilers to C and CUDA. Reported: 400 MIPS on one M3 Max thread, 5,200
  MIPS on 16 threads, 74,000 MIPS on an RTX 4090 with 32,768 threads. Presented
  at the FProPer workshop at ICFP 2024.
- **Bend**: a Python-shaped surface language on top of HVM2.
- **A paper**, in the repository, marked work in progress.
- **Attention**: about 13,000 likes on the launch post, coverage by Fireship and
  ThePrimeTimeagen, and today 19,790 stars on Bend and 11,339 on HVM2.

That is a real result. Running closures and unrestricted recursion on a GPU had
not been done. The near-linear scaling with core count was reproducible and
nobody disproved it.

### 2.3 Funding

Taelin's account: he pitched interaction combinators as fundamental technology
that was years from a product, and expected the pitch to fail. It closed in
about a week at $5m; one investor withdrew, leaving $4m. He then judges the seed
stage as inefficient — money spent on people without the conditions to be
productive, most results produced by a few people — while stating the technical
milestones were met and about 60% of the money was spent with two years of
runway left.

### 2.4 What happened afterwards

Measured today (2026-08-11):

| Artifact | Last push | Note |
|---|---|---|
| HVM2 (repo renamed from HVM) | 2024-11-21 | dormant for about 20 months |
| Bend | 2026-07-07 | last functional commit 2025-06-03; the 2026 commit message is `chore: bump repo activity` |
| Kind (proof language) | 2025-01-22 | dormant |
| Bend2 | 2025-06-17 | personal repo, work in progress, 13 stars |
| HVM3 | 2026-01-29 | Haskell + C, sequential, positioned as a symbolic-computation engine |
| HVM4 | 2026-05-30 | single C file, builds with one `clang -O2`, README says it is pre-launch. Read directly below |

The direction changed. HVM3's README leads with the Interaction Calculus,
linearity, superpositions and optimal beta reduction, and mentions parallelism
only as a consequence of affinity. HVM4 drops the parallel headline entirely;
its documented flags are about collapsing superpositions, which is program
search. The publicised follow-on work (NeoGen, Bend2) uses optimal evaluation to
search program space, not to fill a GPU.

Taelin's own output moved further. His recent repositories are AI agent tooling
and an LLM benchmark of pure lambda calculus tasks. In January 2025 he wrote
that he had stopped trying to persuade people to use interaction nets.

Read plainly: **the massively-parallel-runtime bet was not sustained, and the
sharing bet was.** The part that survived is the part where interaction nets are
asymptotically better — search over shared program space — not the part where
they compete with a register machine.

---

## HVM4 — the current line, read directly

HVM4 is the live repository, so it is the best evidence of what HOC believes
today. It is not a product. It has no license file and the README states that
the reader arrived before launch.

### Shape

First commit 2025-10-26, 560 commits, last push 2026-05-30. Contributors are
Taelin and HOC people (nicolas-abril, Lorenzobattistela, pjcavalcanti), so this
is not a solo repository — it is the only HOC repository with current work.

- `src/hvm.c` — 6,435 lines, the whole runtime. Build: one `clang -O2`.
- `docs/` — a primer, the Interaction Calculus theory, core AST, syntax, memory
  layout, the collapser, and 64 files with one interaction rule each.
- `devs/test/` — 219 test files and a shell runner.
- `devs/bench/` — six programs, one with a TypeScript twin (`lambda_eval.ts`).
- `devs/issues/` — open bug repros, checked in: a dynamic-dup bug, a fork syntax
  bug, and an interpreter-against-compiler out-of-memory difference.
- `AGENTS.md` and `CLAUDE.md` — the repo is set up for AI-assisted work.

`hvm.c` is organised in labelled sections: Types (term tags, bit layout), Term,
Heap, Nick/Table/Print, Parse, WNF (the stack evaluator and the interaction
rules), Data, CNF (readback), Eval, CLI. Each interaction rule in the C file has
a matching markdown file. Terms are one packed word: substitution flag, tag,
label, value. Hot tags come first: APP, VAR, LAM, DP0, DP1, SUP, DUP, ALO.

### What changed against HVM2 and HVM3

The direction reversed inside this repository, and the dates are exact.

- March 2026: an ahead-of-time compiler landed (`clang/aot/emit.c`, 2,168
  lines) — the thing HVM2 promised and never had.
- 1 April 2026: a CUDA runtime landed, then two throughput commits, 4 → 22.7 →
  23.6 GIPS by memory coalescing and a circular heap, plus a semi-space copying
  garbage collector for bounded memory on tail recursion.
- 7 May 2026: one commit, "Refactor HVM runtime into pure single source",
  removed 15,077 lines: the whole `clang/` tree (211 files — the AOT compiler,
  work-stealing queues, thread counters, FFI, the collector) and the whole
  `cuda/` directory (8 files, including `hvm.cu` and its benchmarks). A second
  commit removed the garbage collector.

So the current HVM4 is single-threaded, has no GPU path, no AOT compiler, no
FFI and no collector. All four existed five weeks earlier and were deleted on
purpose. Laziness came back as well: HVM2 reduced every redex eagerly; the HVM4
primer describes lazy evaluation with sharing that extends inside lambdas.

### What the surface says about the thesis

The features that grew are the search features:

- `-C10` runs collapse mode: it enumerates one term as a stream of ordinary
  lambda terms. Same-label superpositions annihilate pairwise; different labels
  make a cross product. That is a search space with shared work.
- `↑` is a priority operator that exists only to order collapse output.
- The last commit in the repository adds a "filter-credit scheduler" for `-C`
  search ordering.
- The language gained `===` structural equality, short-circuit `.&.` and `.|.`,
  unscoped bindings, and fork syntax that captures the variables in scope.

Duplication is now explicit in the syntax and carries a label: `λ&x`, `!x&L=v`,
`&L{a,b}`, and dynamic forms where the label is computed. Variables are affine.
About 25 tests exercise automatic duplication, and a dynamic-dup bug repro is
still open in `devs/issues`. So HVM2's soundness problem is being attacked by
making duplication visible and labelled, not by adding Lamping's oracle.

The plain reading: HVM4 is a reference implementation of an optimal-sharing
symbolic engine for program search. Parallelism is not the product any more. It
was built here, measured here, and cut here.

### Does this change the takeaways?

Two of them get stronger, none reverses.

- §4.3 holds harder. The surviving thesis is "share the search, do not expand
  it". It is now the entire product, not a side benefit. Vyrn's DFA containment
  walk is the same principle in a conventional compiler.
- §4.4 gains a real convergence. HOC deleted its second and third backends —
  CUDA and AOT — five weeks after building them, to keep one runtime that is
  true. Vyrn deleted the Inkwell backend for the same reason and recorded the
  same lesson: ungated multiplicity rots. Two projects with opposite execution
  models reached the same rule. Note the difference that matters: Vyrn keeps
  three backends because a parity harness gates them; HOC had no gate, so the
  only safe number of runtimes was one.

Nothing in HVM4 changes §4.1, §4.2 or the rejected angles. If anything it
supports the rejection of a GPU target: HOC wrote a working CUDA runtime,
tuned it, and then removed it.

---

## 3. The benchmark record

This is the part with direct transfer value, so it is given in detail.

### 3.1 What was claimed

Bend's README: if your code can run in parallel, it will run in parallel; no
threads, locks, mutexes or atomics. The headline table:

- Bitonic sort, Rust interpreter, M3 Max: 12.15 s
- Bitonic sort, C interpreter, M3 Max: 0.96 s
- Bitonic sort, CUDA, RTX 4090: 0.21 s

### 3.2 Fault one: the table compares the system to itself

All three rows are Bend running Bend. The speedup measured is against Bend's own
slowest interpreter. No external language appears. A reader who wants to know
whether to use Bend cannot answer that question from the headline table.

### 3.3 Fault two: the chosen example was the worst case

The guide's example was a recursive `sum`. Readers ported it within hours:

- Python 3.12: about 1 m 42 s. PyPy: about 4.5 s.
- GHC 8.8, single thread, four-year-old laptop: about 2.5 s.
- Bend: minutes.

The cause is exactly the cost model in §1.5 — Python and GHC do no allocation in
that loop; Bend allocates two nodes per numeric operation and builds a recursive
stack. Taelin's reply during the thread called the choice of `sum` a large
mistake, and he replaced the README example with bitonic sort. By then the
audience had formed its judgement.

A commenter proposed the correct practice: publish a weak case beside a strong
case, and explain why. Taelin accepted it. It was never adopted.

### 3.4 Fault three: an incorrect number reached the README

The published single-thread versus 16-thread comparison summed 2^30 numbers.
HVM2's address space holds 2^29 nodes. The single-thread run was slowed by
memory exhaustion, which inflated the speedup. Corrected during the thread with
2^28: 33.39 s versus 2.94 s. The live demo made it worse — the default `run`
selected the Rust interpreter, so the single-thread baseline was the slower
runtime. The restated speedup was about 12x, not about 16x.

The pattern is worth naming: **the limits were documented, and the benchmark
crossed them anyway.** A limit written in a limitations section does not protect
a number produced in a different file.

### 3.5 Fault four: an unconvertible unit as the headline

The paper's headline is MIPS — millions of interactions per second. It is the
correct unit inside the literature, and it is the correct unit for measuring the
evaluator against itself. It is also unconvertible: an interaction is not a
FLOP, an instruction, or a work item.

When a commenter asked for a familiar unit, Taelin refused on the grounds that
inventing a conversion would be untruthful. He was right about the conversion
and wrong about the consequence. A headline number the reader cannot check
against anything they know is not a measurement to them; it is an assertion. The
answer he gave later in the same thread is the correct one, and should have been
the headline: compare wall-clock time on the same program.

### 3.6 Fault five: the paper's benchmark section is empty

The paper source in the repository today still contains, in full, under the
section heading `= Benchmarks`, the line `TODO: include some benchmarks`. The
abstract carries the three MIPS figures. So the document that made the
performance claim never contained the evidence section, from launch to now.

### 3.7 Third-party measurement

Speedfox, September 2024: matrix determinant by cofactors, Bend against Go, on
an AMD FX-4130. At 7x7 Bend was competitive. At 11x11 Bend took about 2 hours
single-threaded and about 20 minutes parallel; Go took 22 seconds and 4 seconds.
The author traced it to Bend's list indexing being O(i). Conclusion: automatic
parallelism was delivered, and it bought neither productivity nor speed.

### 3.8 The defence, and why it did not work

Taelin's position was consistent: the only claim was linear scaling with cores;
the disclaimers were in the README, the guide and the paper. All of that is
true. The README did state that the code generator was immature. The paper did
state the 5x-to-100x single-thread deficit against GHC.

It did not work because of placement. The disclaimer sat below the install
instructions; the numbers sat above them. A reader was asked to hold a caveat
they had not read yet. One commenter said so directly, and Taelin agreed to move
it. The lesson is mechanical, not moral: **a caveat must precede the number it
qualifies, in the same block of text.**

---

## 4. Takeaways for Vyrn

### 4.1 Two bets on deterministic parallelism (holds)

Both projects promise the same user-visible property: the result does not depend
on the schedule. They buy it in opposite places.

**HVM buys it in the execution model.** Confluence is a theorem about the
rewrite system. The price is paid on every operation: no registers, no arrays,
no mutation, no laziness, no existing optimizer, and one unsolved soundness
condition inherited from dropping the oracle.

**Vyrn buys it in the checker.** RFC-0004 and RFC-0025: a spawned function is
proven isolated, transitively — no effects, no module state, no I/O, no drop of
shared cells. The only observable is the return value, so any schedule gives
byte-identical output. Parallelism is then wall-clock only, and the three-way
parity harness is unaffected by it. The price is paid once, at the `spawn`
boundary, in what you may put inside a task.

The comparison to state, when Vyrn's concurrency story is described: HVM makes
every operation pay for a property that most operations do not need. Vyrn
charges the operations that need it. For a systems language, that is the right
side of the trade, and Vyrn should say so with this example rather than in the
abstract.

One honest concession. HVM finds parallelism the programmer did not write; Vyrn
requires `spawn`. If implicit parallelism is ever wanted, the transferable
design is: keep the runtime, and let the compiler auto-`spawn` independent calls
that the isolation analysis already proves isolated. The analysis exists; the
threading exists; the missing part is a cost model good enough to decide when a
task is worth a thread. Do not build it before that cost model can be measured.
Bend shows the failure mode of shipping automatic parallelism on top of a
sequential baseline nobody has tuned yet.

### 4.2 Benchmark honesty as a positioning asset (holds — with an action)

The launch damage came from a benchmark table, not from the technology. The five
faults in §3 are all avoidable by rule, and Vyrn already owns most of the
machinery to obey them: three-way parity that is byte-identical including trap
text, a bench harness with `--json`/`--compare` in CI, and a habit of recording
measured numbers instead of adjectives.

Rules to make explicit, each earned from a specific fault above:

1. **Never headline a self-comparison.** A table in which every row is Vyrn
   measures Vyrn's own worst configuration. Every performance claim gets at
   least one external baseline the reader already trusts — `clang -O2`, Rust,
   Node, GHC.
2. **Publish a weak case beside the strong case.** Bend's own audience proposed
   this and it would have cost nothing.
3. **Put the caveat above the number, in the same block.** Not in the guide, not
   below the install steps.
4. **State the machine, the flags, and the input size next to the number.**
5. **State any limit where the number is, not only in a limitations section.**
   The 2^29 node ceiling was documented and still produced a wrong README.
6. **Internal units are for regression only.** Vyrn's bench counters are correct
   for tracking Vyrn against Vyrn over time. The public claim is wall-clock on a
   program a sceptic can port.

Action for this repo: the README carries claims of the same class as Bend's —
memory measured flat at about 3 MB against 1.2 GB for the same loop, and speed
claims recorded across the design notes. These are believable and probably
correct. They currently lack the machine, the flags and the competing baseline
inline. Adding those three items converts them from assertions into results, and
costs one line each.

### 4.3 Does any interaction-net idea pay inside Vyrn? (mostly rejected)

**Rejected: optimal beta reduction in the runtime.** Vyrn monomorphizes
generics, defunctionalizes closures into closed enums with direct calls, and
compiles ahead of time. There is no run-time beta reduction to be optimal about.
The one property optimal reduction buys — never duplicating unevaluated work —
is bought in Vyrn at compile time and for free.

**Rejected: interaction nets as an IR for `spawn`/`join`.** Vyrn's task
granularity is a whole function on a real thread. Interaction granularity is a
few pointer writes. Adopting it would replace a proof-based guarantee that costs
nothing with a runtime guarantee that costs everything.

**Rejected: the affine, one-duplicator discipline.** Vyrn's ownership model
already makes duplication explicit and already gets the memory result. Copying
HVM's restriction would import its soundness hole and give nothing back.

**Holds, narrowly: sharing inside the comptime evaluator.** `gen fn` runs pure
code at compile time, and RFC-0021 caches at whole-call granularity, keyed by
`sha256(generator sources ++ args)`; finer-grained regeneration is listed as
deferred. The transferable idea from HVM is the plain one — do not re-evaluate
equal structure, memoize by structure — not optimal reduction itself. Note the
measured history first: the large comptime win so far came from RFC-0076 moving
generators to compiled wasm, and from removing a quadratic string append worth
240x. Both were stupid costs, not evaluation-order costs. So this stays on the
"only if measured" list.

**Holds, as a confirmation rather than a change: share the search, do not expand
it.** Taelin's most durable result is that superposed terms let one evaluation
cover many candidate programs by sharing common work — which is why HVM3 and
HVM4 kept the collapse machinery and dropped the GPU headline. Vyrn already
applied the same principle where it counted: finite-string containment walks a
product automaton instead of expanding the union cross-product that TypeScript
builds. That is worth stating as a design value, because it is the one place the
two projects agree.

### 4.4 Solo visionary against incremental record (holds)

Do not dismiss the solo model. It produced a result nobody else was going to
produce, a runtime that executes closures on a GPU, a paper, tens of thousands
of stars, and $4m on a pitch whose explicit content was *this is not a product
yet*. Ten years of unpaid work on an unfashionable algorithm is the reason any
of it exists.

Then count the cost. One person set the release date, chose the benchmark
program, wrote the README number and answered every critic, in the same week.
The seed retrospective admits the money did not convert into output. And when
the founder's attention moved, the artifacts stopped: HVM2 unpushed for 20
months, Bend's only 2026 commit exists to bump activity, Kind idle since January
2025, Bend2 a 13-star work in progress. The line continued only where he
personally worked.

The counterweight in Vyrn is already built and should be treated as the
strategic asset, not as overhead:

- The RFC record makes a claim checkable years later, including the claims that
  turned out wrong.
- The parity harness makes a regression fail rather than get argued about.
- The recorded lesson that gated multiplicity stays true and ungated
  multiplicity rots — the deleted Inkwell backend — is exactly the discipline
  HOC lacked when two runtimes and a language drifted apart.
- The recorded cases where a claim died to measurement (the builtin-declaration
  census landing 45:38 against its own thesis, an M1 that failed its own line
  gate and was merged anyway) are the evidence that the record is real. HOC has
  no artifact of that kind, because the paper's benchmark section is a TODO —
  there is nothing there that could fail.

The rule to carry: **a claim that cannot fail a check is not a claim.** Vyrn's
equivalent of HVM's empty benchmark section would be a performance sentence in
the README with no harness behind it.

On timing, the correct reading is not "wait longer before releasing". Bend's
parallel scaling was real and defensible on release day. The code generator was
not, and Taelin said so in three places. The error was welding them into one
table. Release the part that holds; do not attach a performance headline to the
part you know is immature.

### 4.5 Angles considered and rejected

- **"Adopt automatic parallelism, because HVM proves it works."** Rejected.
  HVM's automatic parallelism is a consequence of paying an allocation for every
  operation. Vyrn's cost model is the opposite by design.
- **"Target GPUs."** Rejected. Bend's GPU path needed a 4 GB address space,
  24-bit numbers and a specific card with at least 96 KB of L1 per SM. Vyrn's
  workloads — compilers, servers, UI, CLI tools — are not GPU shaped, and Vyrn
  already has two backends to keep at parity.
- **"Attention is the growth strategy."** Rejected on the evidence. 19,790 stars
  did not produce maintainers. The repository is now kept alive by a commit whose
  message is that it is keeping the repository alive.
- **"Optimality is a marketable property."** Rejected. Optimality here is a
  statement about beta-step counts. It did not predict wall-clock, and defending
  it consumed the launch. Vyrn's marketable properties — byte-identical parity
  across three backends, no GC, deterministic concurrency — are checkable by a
  reader in one command, which is the property that matters.

---

## Sources

Read 2026-08-11.

- Taelin personal README —
  `https://raw.githubusercontent.com/VictorTaelin/VictorTaelin/refs/heads/main/README.md`.
  It is a link index. The retrospective content is in the linked gists, not in
  the README itself.
- HOC complete historical overview (the candid document: Lamping, the oracle,
  Absal, FM-Net, HVM1, the funding account) —
  `https://gist.github.com/VictorTaelin/77fd5a2a8a4a07e1da6157ebca3c7cf1`.
- HOC: towards an optimal computer —
  `https://gist.github.com/VictorTaelin/46936b9fdfc3f982f07963c11756e36b`.
- Optimal evaluation in 10 minutes —
  `https://gist.github.com/VictorTaelin/311f6a58a7756945196c15733e61d0c6`.
- HVM2 paper, source form (limitations, empty benchmark section) —
  `https://raw.githubusercontent.com/HigherOrderCO/HVM/main/paper/HVM2.typst`.
  PDF at `paper/HVM2.pdf` in the same repository.
- Bend README and FAQ —
  `https://raw.githubusercontent.com/HigherOrderCO/Bend/main/README.md`,
  `.../FAQ.md`.
- HVM3 — `HigherOrderCO/HVM3` (Haskell front end, C runtime, `IC.md`, `HVM.md`).
- HVM4 — `HigherOrderCO/HVM4`. Read: `README.md`, `AGENTS.md`, `docs/primer.md`,
  `docs/hvm/collapser.md`, the repository tree, the section banners of
  `src/hvm.c`, and the commit log. The deletion commit is
  `4bc2336dc83e118f1d36338826564d7472452829` ("Refactor HVM runtime into pure
  single source", 2026-05-07, +7,469 / -15,077).
- Launch discussion, 246 comments, 34 of them from Taelin (handle
  `LightMachine`) — `https://news.ycombinator.com/item?id=40390287`. Machine
  readable: `http://hn.algolia.com/api/v1/items/40390287`.
- Third-party benchmark, September 2024 — Speedfox, "Breaking Bend: Benchmarking
  the HVM", `https://blog.speedfox.co.uk/`.
- Papers referenced by the above: Lamping 1990 (optimal reduction), Lafont 1990
  and 1997 (interaction nets, interaction combinators), Asperti/Lawall/Mairson
  1996 (the misread negative result), Asperti's BOHM.

### Reproducing the repository facts

```sh
gh api repos/HigherOrderCO/HVM  --jq '{n:.full_name,p:.pushed_at,s:.stargazers_count}'
gh api repos/HigherOrderCO/Bend --jq '{n:.full_name,p:.pushed_at,s:.stargazers_count}'
gh api "repos/HigherOrderCO/Bend/commits?per_page=5" --jq '.[]|"\(.commit.author.date) \(.commit.message)"'
gh api "orgs/HigherOrderCO/repos?sort=pushed" --jq '.[]|"\(.pushed_at) \(.name)"'
curl -sL https://raw.githubusercontent.com/HigherOrderCO/HVM/main/paper/HVM2.typst | grep -A2 '^= Benchmarks'
curl -sL https://raw.githubusercontent.com/HigherOrderCO/HVM4/main/src/hvm.c | grep -c -iE 'pthread|cuda'
gh api "repos/HigherOrderCO/HVM4/git/trees/main?recursive=1" --jq '.tree[].path'
gh api repos/HigherOrderCO/HVM4/commits/4bc2336dc83e118f1d36338826564d7472452829 --jq '.stats, (.files[]|"\(.status) \(.filename)")'
```
