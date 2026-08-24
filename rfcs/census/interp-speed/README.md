# Making the interpreter faster: what the evidence says

Five files. Three are literature research with citations; two are measurements
taken in this repository. They were produced independently and they agree, which
is worth saying because the agreement is what makes the conclusion usable.

| file | what it is |
| --- | --- |
| `what-a-node-costs.md` | measured here: what one interpreted operation costs |
| `frames-audit.md` | measured here: every place the interpreter opens a scope frame, and whether a compiler could predict it |
| `slots.md` | how shipping implementations resolve names to indices, and what goes wrong |
| `bytecode.md` | tree-walker against bytecode against closure compilation, with numbers |
| `comptime.md` | what Zig, Nim, Rust, D and Clang did about slow compile-time execution |

## The short answer

**Do not resolve names to slot indices. Do not write a bytecode VM. Neither is
where the time is.**

## Why not slot resolution

Measured here, after the work in `../interpreter-loop-cost.md`:

| operand in an expression | cost |
| --- | --- |
| `Expr::Int(1)` — a match arm and a return | **25.0 ns** |
| `Expr::Var("b")` — the same, plus finding the name | **25.8 ns** |

**Finding the name is 0.8 ns of a 25.8 ns read.** Walking one more scope frame
costs about 1 ns, and the whole workload has about 153 million probes that a
`(depth, index)` stamp would remove — roughly 0.15 seconds of a 31-second run.

The literature agrees from the other direction. Every measured win for this
change deleted a **hash**: Boa PR #1829 (−33.6%), Cloudflare wirefilter (2,548 →
1,227 ns/iter). Vyrn already banked that win when `Frame` stopped being a
`HashMap` — 46.8 s to 31.0 s. `slots.md` records **NOT FOUND** for any published
number for adding slot resolution on top of an already index-free short scan.

And it would be the risky kind. Every shipping implementation surveyed — clox,
Lua, Wren, CPython, YARV, starlark-rust — builds ONE picture, either in a single
pass or by only opening scopes at function boundaries. Nobody keeps a static
picture in step with a drifting dynamic one. The one two-pass design in the
survey is jlox, and its author warns about exactly this: *"each line of code that
touches a scope must have its exact match in the interpreter… I ran into a couple
of subtle bugs where the resolver and interpreter code were slightly out of
sync."* No implementation surveyed can detect a wrong-variable resolution at run
time; an assert catches out-of-range and nothing else.

Vyrn has two specific reasons it would drift, found independently in the tree and
by the research:

- `interp.rs:3096` pushes a block's frame **conditionally**, on whether the block
  holds a `Stmt::Let`. A resolver would have to duplicate that predicate exactly,
  and keep doing so.
- `project.rs:372` clones a projection body and **renames its bindings with a
  fresh counter on every call, while the program runs**. Slots stamped on the
  original AST do not survive it.

## Why not a bytecode VM

The headline multiples people quote — Monkey 4.5x, Lox 6x, Ruby 1.9 4x — are all
against an untuned tree-walker. Against a *tuned* one the only clean measurement
in `bytecode.md` is GoAWK: **13–18 per cent for 2,500 added lines**, and its
author wrote that he was "not entirely sure it was worth" them. One surveyed case
had the bytecode VM come out **2–3x slower** than the AST walker it replaced.

The comptime evidence is worse, and it is the evidence that matters most here
because compile-time execution is what this interpreter is for:

- **D's newCTFE**: 15 months, about 12,000 lines, never merged. The branch has
  been frozen since 2017.
- **Clang's bytecode constant interpreter**: started 2019, still behind
  `-fexperimental-new-constant-interpreter` in 2026 — and **slower than the tree
  evaluator on array initialisation without function calls**, which is the
  closest shape to Vyrn's byte-copy loops.
- Clang ships **two** back ends on purpose: bytecode for function bodies,
  direct evaluation for top-level expressions, "since the bytecode would never be
  reused". A generator runs once per build.

Clang's two constant evaluators have also disagreed with each other since 2019
and still do. Vyrn's oracle is stricter than theirs: byte-identical output,
traps included, across three engines.

## What did work, everywhere

**Fixing how values are represented**, not how they are executed.

- **Nim** closed a compile-time array copy that took 237 seconds with **four
  lines** in `vmgen.nim`, stopping big constants being re-inlined on every read.
- **Zig** diagnosed its comptime blow-up as "we need to model comptime-mutable
  memory as actual mutable memory"; the fix was `MutableValue`.
- **D's** cause, per the person who worked on it, is that the AST interpreter
  "needs to copy every variable on every mutation… a deep-copy". The issue has
  been open since 2011.
- **Nederlang**, a tree-walker in Rust, went 39.3 s to 1.8 s on `fib(35)` — 21.8x
  — without ever becoming a VM. The steps were representation: hashing, `Vec`
  environments, borrowed AST nodes, tagged values. 46 per cent of its time was
  cloning the value enum.

That is the same lever this project already pulled, and it is where the 62 s to
24 s came from.

## Two experiments here that failed

Both are in the files, because a negative result that took a measurement is worth
more than an untested opinion.

- **Splitting `Frame` into `names: Vec<String>` and `slots: Vec<Slot>`.** A
  `Slot` is 96 bytes, so three entries span six cache lines and every name
  comparison reaches a different one; three name headers fit in about one. It was
  **4 per cent slower** — two vectors mean two allocations and two pointer
  chases.
- **Boxing `Val::Enum`**, the widest variant, taking `Val` from 48 bytes to 32
  and what every node returns from 56 to 40. No measurable change: 29.6 s against
  29.7 s. Nederlang's 46-per-cent figure does not transfer, because the values
  that are expensive to copy here are already behind an `Rc`.

The second one is the useful negative: the data-representation seam that paid so
well is now closed. What is left in a 25 ns node is the dispatch.

## The one option still open

**Closure compilation** — walk the AST once and build a tree of Rust closures, so
the match and the name lookup happen when the tree is built rather than on every
execution.

It is the only architectural change with a positive measurement in Rust, and it
is a striking one. Same workload, same author, all Rust: AST walk 2.1 s,
**closures 1.4 s**, instruction stream 1.5 s, byte-encoded 1.5 s. Closures took
all of the available win; byte-encoding added nothing.

Two things make it cheaper here than it would be elsewhere:

- **`vyrn-lower` already exists and already borrows.** Its own header says the
  borrow is what "lets an engine migrate one arm at a time and fall back to its
  old walk for the rest" (`vyrn-lower/src/lib.rs:16`). The staged-migration
  harness is built.
- The migration is per node kind, so parity can be proved after each arm rather
  than after a rewrite.

The counterweight, and it is real: starlark-rust shipped closures, then added
bytecode eighteen months later and **published no delta**. `bytecode.md` records
that measurement as **NOT FOUND**, and it is the one number that would change
this ranking.

## On the builtin question, which is the owner's

`comptime.md` answers something the earlier `slice` write-up left open.

Native fast paths in Nim, D, Clang and Rust are **not a speed measure**. Every
one of those tables exists because the function has no interpretable body — Nim
states the rule in a comment: importc a symbol only when its body is empty,
otherwise run the body. No project surveyed put a string, array or copy routine
in such a table to make working code faster.

Nim did once have a genuine speed fast path, `hashVmImpl`. It is **dead code**:
its call sites were commented out by an unrelated change and the registration was
never removed. That is the measured cost of a second implementation.

So: an interpreter fast path that binds to the project's single existing
definition is not a second implementation. One that restates the behaviour in
Rust is, and it rots. For a Vyrn-written byte loop there is no existing
definition to bind to — which means the honest shape, if that road is taken, is
a builtin with no Vyrn body, exactly what `slice` was before RFC-0078.

`RECOMMENDATION, NOT A DECISION` throughout. Each file carries its own, with
citations, and each marks what it could not find.
