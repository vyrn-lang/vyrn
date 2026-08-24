# Bytecode VM, closure compilation, or neither

Research for the Vyrn interpreter decision. Every claim carries a citation. Where
no evidence was found, the line says `NOT FOUND`.

Read-only. No file in `N:/wt-bytefix` or `N:/lang` was changed.

---

## 0. The starting position, from the repository

These are the numbers the decision starts from.

- The interpreter is 10,051 lines (`wc -l` on
  `N:/wt-bytefix/compiler/vyrn-frontend/src/interp.rs`).
- The longest CI step went 46.8 s to 31.1 s
  (`N:/wt-bytefix/rfcs/census/interpreter-loop-cost.md:88`).
- An empty interpreted loop iteration was 167 ns before that work
  (`N:/wt-bytefix/rfcs/census/interpreter-loop-cost.md:119`).
- 653 million frame probes still find something. The census names the next step
  itself: resolve each `Expr::Var` to a `(depth, index)` at check time
  (`N:/wt-bytefix/rfcs/census/interpreter-loop-cost.md:109`).
- A cache-layout change that "should have worked" was 4 per cent slower and was
  reverted (`N:/wt-bytefix/rfcs/census/interpreter-loop-cost.md:101`).
- The evaluator already borrows the AST. `fn expr(&self, expr: &Expr, scope:
  &mut Vec<Frame>)`
  (`N:/wt-bytefix/compiler/vyrn-frontend/src/interp.rs:4410`). It does not clone
  AST nodes to walk them. This matters for section 6.
- Values are already copy-on-write behind `Rc`: `Val::Str`, `Val::Record`,
  `Val::Array`, `Val::Map`
  (`N:/wt-bytefix/compiler/vyrn-frontend/src/interp.rs:418`, `:447`, `:459`,
  `:470`).
- A lowered form already exists and already carries a type and a line on every
  node, and already **borrows the `&Expr` it came from** so that "an engine
  migrate one arm at a time and fall back to its old walk for the rest"
  (`N:/wt-bytefix/compiler/vyrn-lower/src/lib.rs:16`). Today `vyrn-codegen`
  consumes it. `interp.rs` mentions it once, in a trace
  (`N:/wt-bytefix/compiler/vyrn-frontend/src/interp.rs:3223`).

---

## 1. Measured tree-walker to bytecode speedups

### Monkey, Go, same author, same language, both engines

`fib(35)`: evaluator 22.99 s, VM 5.06 s. About 4.5x.
Source: chapter 10 of *Writing A Compiler In Go*, reported through search of the
book text — https://compilerbook.com/ . I did not read the book directly. Treat
the exact seconds as second-hand; the ~4.5x figure is repeated consistently.

This is the cleanest comparison available: one author, one language, two
engines, one benchmark. It is also a recursive-call microbenchmark, which
flatters a VM.

### Lox: jlox (Java tree-walk) against clox (C bytecode)

LoxLox, a Lox interpreter written in Lox, runs about 6x faster under clox than
under jlox — https://benhoyt.com/writings/loxlox/ .

Note what this is not: jlox is Java, clox is C. The 6x mixes host language,
value representation and engine architecture. It is not a clean bytecode
measurement. *Crafting Interpreters* itself argues the architecture, not a
number: "the whole family of AST classes ... reduced down to three arrays" —
https://craftinginterpreters.com/chunks-of-bytecode.html .

A published, side-by-side benchmark table of jlox against clox on identical
hardware: **NOT FOUND**.

### Ruby 1.8 to 1.9 (YARV)

"1.9 on average four times faster than the original interpreter", attributed to
Antonio Cangiano's benchmarks — https://en.wikipedia.org/wiki/YARV .

Caution, and it is a real one: Ruby 1.9 shipped many changes besides YARV. A
citation that isolates the VM from the rest of the 1.9 release: **NOT FOUND**.
So 4x is a release figure, not a VM figure.

### GoAWK, Go, tree-walker to bytecode VM — the honest small case

- 18 per cent faster on microbenchmarks overall.
- 13 per cent faster on realistic benchmarks.
- Cost: 2,500 lines added to a 15,000-line project.
- The author: "I'm not entirely sure it was worth the additional 2500 lines of
  code."

https://benhoyt.com/writings/goawk-compiler-vm/

This is the most directly comparable case in this report. GoAWK's tree-walker
was already tuned. The win was 13 per cent, not 4x.

### A bytecode VM that came out slower

Two PEG interpreters in TypeScript, same author, ~600 lines total. The
tree-walker beat the bytecode VM on every benchmark:

- Node/V8: 2.86x, 2.24x, 2.04x in the tree-walker's favour.
- Bun/JSC: 3.69x, 3.20x, 3.17x in the tree-walker's favour.

"My bytecode interpreter turned out to be slower! In fact, the tree-walking
interpreter is about 2–3x faster on my benchmarks."
https://dubroy.com/blog/two-little-interpreters/

The author does not explain why: "that's a project for another day." So this is
evidence that the win is not automatic, not evidence of a mechanism.

### Reading of section 1

The 4x-to-6x figures come from cases where the tree-walker was slow to begin
with, or where the host language changed at the same time. Where the tree-walker
was already tuned (GoAWK), bytecode bought 13 per cent. Where the host runtime
optimises polymorphic calls well (JS engines), bytecode lost.

---

## 2. Closure compilation — the option to cost

### What it is

Walk the AST once. Emit a tree of host-language closures. Name lookup, arity,
dispatch and constant folding are resolved when the tree is built, not on every
execution. "Walk the tree only once, as if we were to compile it, but instead of
producing a list of instructions, generate a chain of suspended function calls."
https://pl-rants.net/posts/compile-to-closures/

### The measurement that matters most here — and it is in Rust

Neil Mitchell benchmarked four engines for the same workload, all in **Rust**:

| Engine | Time |
| --- | --- |
| Interpret the AST directly | 2.1 s |
| Compile the AST to closures | **1.4 s** |
| Compile the AST to a stream of instructions | 1.5 s |
| Encode those instructions as bytes | 1.5 s |

http://neilmitchell.blogspot.com/2020/04/writing-fast-interpreter.html

Read that table twice. In Rust, closures beat both bytecode forms. Bytecode was
1.5 s; closures were 1.4 s; the AST walk was 2.1 s. Closure compilation took
**100 per cent** of the available win in that experiment, and the byte-encoding
step bought nothing at all.

Caveat, stated by the author: the workload is small — variable assignments and a
while loop doing arithmetic 100 times. Native Rust does the same work in 0.003 s.
One workload, one shape. Do not generalise it to a 4x claim.

The mechanism the author gives: the closure approach "trades matching on the AST
for an indirect function call at runtime", and Rust turns those tail calls into
jumps the branch predictor handles well.

### A second measurement, different host

RTypes, Elixir on the BEAM. Compiled-to-closures checker against an AST
interpreter:

- Simple term: 544.60 ips against 272.75 ips.
- Complex term: 826.93 ips against 393.51 ips.

Roughly 2x on both. https://pl-rants.net/posts/compile-to-closures/

### What the projects actually did — and this is the part to weigh

`starlark-rust` (Meta, Buck/Bazel's configuration language, written in Rust) is
the project behind Mitchell's benchmark. Its history:

- 0.5.0, August 2021: "There have been many changes since the last release,
  primarily focused on performance (**up to 100x in some benchmarks**)."
- 0.6.0, November 2021: "**Addition of a bytecode interpreter**, with associated
  performance gains." Also "Constant propagation and speculative execution during
  compilation."
- 0.7.0, March 2022: "Many optimisations to the bytecode compiler."

https://raw.githubusercontent.com/facebook/starlark-rust/main/CHANGELOG.md

Today the repository has both `starlark/src/eval/compiler/` and
`starlark/src/eval/bc/` —
https://github.com/facebook/starlark-rust/tree/main/starlark/src/eval

So the sequence was: AST walk, then closures, then bytecode. Two facts follow,
and they point in opposite directions.

1. The 100x release is the one **before** bytecode. The largest published
   speedup came from the closure era plus representation work.
2. They did eventually add bytecode. The changelog gives no number for it —
   "associated performance gains" is unquantified. A measured
   closures-against-bytecode figure for starlark-rust: **NOT FOUND**.

### Answer to the question asked

Does closure compilation get most of a bytecode VM's win, or a small part?

On the only Rust measurement found, it got **all of it and slightly more**
(1.4 s against 1.5 s, from a 2.1 s baseline) —
http://neilmitchell.blogspot.com/2020/04/writing-fast-interpreter.html . On the
BEAM it got about 2x — https://pl-rants.net/posts/compile-to-closures/ . The
team that measured the Rust case still went to bytecode eighteen months later
and did not publish the delta.

A study that isolates "closures against bytecode, same language, same author,
published numbers, non-trivial workload": **NOT FOUND**. That is the single
biggest gap in this report.

---

## 3. Register versus stack bytecode

### The controlled study

Shi, Casey, Ertl and Gregg, *Virtual Machine Showdown: Stack Versus Registers*.
They translated the same JVM programs to both forms:

- The register machine eliminates on average **more than 46 per cent** of
  executed VM instructions.
- Register bytecode is **26 per cent larger**.
- With a C `switch` dispatch on a Pentium 4, the register machine takes on
  average **32.3 per cent less time**.

https://www.scss.tcd.ie/David.Gregg/papers/vee05-ShiGreggBeattyErtl.pdf

Note the qualifier. The 32.3 per cent figure is for switch dispatch. Rust has
only switch dispatch on stable (section 6), so this is the applicable row.

### Lua 5.0, the canonical case

Lua used a stack VM from 1993 and moved to a register VM in Lua 5.0 (2003).
Register code avoids push and pop; all local variables live in registers.
Instructions are 4 bytes against 1–2, but far fewer are emitted, so code size is
not much larger. https://www.lua.org/doc/jucs05.pdf

A published before/after speed number for Lua 4 against Lua 5 attributable to the
register change alone: **NOT FOUND**.

### Guile, stack VM to register VM

Count-to-a-billion loop: Guile 2.0 stack VM 24 s, Guile 2.2 register VM 9 s.
About 2.7x. https://wingolog.org/archives/2013/11/26/a-register-vm-for-guile

Real-world programs: "a speedup of 30% or more" for 2.2 against 2.0 —
https://wingolog.org/archives/2017/03/15/guile-2-2-omg . The 2.7x is a loop
microbenchmark; 30 per cent is the honest program-level figure.

### What a register VM costs the compiler

Wingo says only that "getting this right requires some compiler sophistication",
and that address-space limits forced "compiler emission of some shuffles" —
https://wingolog.org/archives/2013/11/26/a-register-vm-for-guile . He does not
quantify it.

Boa (JavaScript in Rust) moved stack to register in v0.21. The register
allocator "significantly increases the complexity" of the compiler —
https://www.x-cmd.com/blog/251025/ (a third-party write-up, not the Boa team's
own words). A measured line-count or engineer-time cost for Boa's register
migration: **NOT FOUND**. A published Boa 0.20-against-0.21 benchmark table:
**NOT FOUND** in the sources reached.

### Reading of section 3

Register beats stack by roughly 30 per cent on programs, by more on loops, at the
price of a register allocator. That is a second project on top of the first. It
is not a reason to pick a VM; it is a reason to be honest that "a VM" means "a
VM and then a register allocator" if the numbers are to reach the published ones.

---

## 4. Staged migration and dual-engine validation

### The closest match to Vyrn's situation: Clang's constant evaluator

Clang has two constant expression evaluators at once. The old one walks the AST
(`ExprConstant`). The new one compiles to bytecode. They coexist.

- Purpose: "The bytecode interpreter aims to replace the existing AST
  traversal-based evaluator in Clang, improving performance on constructs which
  are executed inefficiently by the evaluator." —
  https://clang.llvm.org/docs/ConstantInterpreter.html
- The RFC that started it is from **July 2019** —
  https://lists.llvm.org/pipermail/cfe-dev/2019-July/062799.html
- As of Clang 23 it is still behind
  `-fexperimental-new-constant-interpreter`, or a build-time cmake flag
  `-DCLANG_USE_EXPERIMENTAL_CONST_INTERP=ON` —
  https://clang.llvm.org/docs/ConstantInterpreter.html
- 308 commits tagged `[clang][Interp]` since November 2022, many of them
  no-functional-change refactors —
  https://developers.redhat.com/articles/2024/10/21/new-constant-expression-interpreter-clang-part-2

Seven years, still opt-in. That is the schedule for "replace the compile-time
evaluator of a real language, keeping the old one as the reference."

And the two engines disagree in practice. LLVM bug 172165: the bytecode
interpreter and `ExprConstant` give different answers when comparing pointers to
struct members. The AST interpreter is correct; the bytecode one is not. The
report: "It seems problem is in `clang/lib/AST/ByteCode/Interp.h` Where
comparison is done thru offsets, but for some reason the first member has offset
16 and not 0." — http://www.mail-archive.com/llvm-bugs@lists.llvm.org/msg94621.html

For Vyrn that bug class is a parity failure across three engines, not one.

### The architectural answer to divergence: generate both from one definition

CPython defines instruction semantics once, in `Python/bytecodes.c`, in a small
C-like DSL. Tools generate the tier-1 interpreter and the tier-2 interpreter from
it, so "there is a single source of truth for bytecode semantics", and other
tools that read bytecode derive from the same definition, "reducing errors" —
https://github.com/python/cpython/blob/main/Tools/cases_generator/interpreter_definition.md

The JIT extends the same idea: translation rules are generated from the DSL, so
"most JIT translations are correct-by-construction" —
https://github.com/python/cpython/blob/main/InternalDocs/jit.md

### Differential testing as the general method

Differential testing uses one implementation as the oracle for another; any
divergence on the same input is a candidate bug. Applied to language engines,
"validating such engines requires not only validating each in isolation, but also
that they are functionally equivalent", and interpreter-guided differential
testing of JIT compilers uses the interpreter's result as the reference baseline
— https://dl.acm.org/doi/10.1145/3519939.3523457 and
https://drops.dagstuhl.de/storage/01oasics/oasics-vol134-programming2025/OASIcs.Programming.2025.20/OASIcs.Programming.2025.20.pdf

Vyrn already runs this arrangement over 40 programs
(`N:/wt-bytefix/compiler/vyrn-cli/tests/parity.rs`, 170 KB).

### The migration mechanism Vyrn already owns

RFC-0101's lowered form borrows the source expression on purpose, and the
comment says why: it "is the only thing that lets an engine migrate one arm at a
time and fall back to its old walk for the rest"
(`N:/wt-bytefix/compiler/vyrn-lower/src/lib.rs:16`). That is a staged migration
harness that already exists and that the two compiled backends already use.

---

## 5. What a VM costs that nobody mentions up front

### Source positions stop being free

In a tree-walker, the node is the position. In bytecode, the mapping has to be
rebuilt and maintained.

- CPython needed **PEP 626** to make line numbers precise again, replacing one
  compressed table with another and adding `co_lines` —
  https://peps.python.org/pep-0626/
- CPython then needed **PEP 657** to get column offsets back, because "a single
  line of Python code can compile into dozens of bytecode operations making it
  hard to track which part of the line caused the error". The fix is a per-
  instruction table of start line, end line, start column and end column,
  exposed as `co_positions` — https://peps.python.org/pep-0657/
- The PEP 657 discussion carried explicit worry that the cost would sink it:
  "nicer locations for errors is great, [but] it won't be popular if it has a
  negative impact on performance" — https://peps.python.org/pep-0657/

Two PEPs and two new tables, to recover information the AST held for nothing.
Vyrn's trap messages must be byte-identical across three engines, so this is
directly on the risk path.

### Errors move from "reject now" to "reject later"

Clang's bytecode interpreter had to invent an "Invalid opcode" mechanism, because
"we can't reject a `constexpr` function right away when generating bytecode for
it" —
https://developers.redhat.com/articles/2024/10/21/new-constant-expression-interpreter-clang-part-2

Compiling to bytecode splits one pass into two, and diagnostics that used to fire
during evaluation now have to be carried through the encoding.

### Value representation stops being free

Clang: "each `APFloat` variable may heap-allocate memory ... This poses a
particular problem for the new interpreter, since values are allocated in a stack
or into a char array." — same source. A VM wants flat, sized slots. Values that
are not flat and not sized need new machinery.

### Size of the change

- GoAWK: 2,500 lines on 15,000 —
  https://benhoyt.com/writings/goawk-compiler-vm/
- Clang: 308 commits since November 2022, on top of work started July 2019, still
  not on by default —
  https://developers.redhat.com/articles/2024/10/21/new-constant-expression-interpreter-clang-part-2

### A single write-up titled as regret

**NOT FOUND**. The nearest is GoAWK's "I'm not entirely sure it was worth the
additional 2500 lines of code" — https://benhoyt.com/writings/goawk-compiler-vm/ .
No published post-mortem of a bytecode VM rewrite that was reverted was located.

### Deoptimization

Deoptimization is a JIT concern, not a bytecode-interpreter concern. No source
was found that charges a plain bytecode VM with a deoptimization cost:
**NOT FOUND**.

---

## 6. Rust-specific

### Dispatch: match against computed goto

On modern high-performance Intel, "dispatch method is not significant performance
differentiator". On ARM and on low-power Intel and AMD it still matters, up to
about 20 per cent. Rust has no portable computed goto —
https://pliniker.github.io/post/dispatchers/

The author publishes his raw timings only in an external spreadsheet; the
per-technique seconds are not in the post. Exact figures: **NOT FOUND** in the
post itself.

A separate dispatch benchmark set records basic switch-based dispatch at
461.57 ms, and states the tail-call technique "is pretty much unusable in current
Rust since Rust is missing guaranteed tail call elimination" —
https://github.com/neopallium/interpreter-dispatch-research

### Tail-call threading, on nightly

With the `become` keyword and `extern "rust-preserve-none"`, a Uxn VM:

| Workload | match | become | Speedup |
| --- | --- | --- | --- |
| Fibonacci, ARM64 | 2.41 ms | 1.19 ms | 2.03x |
| Mandelbrot, ARM64 | 125 ms | 76 ms | 1.64x |
| Fibonacci, x86-64 | 4.70 ms | 3.23 ms | 1.45x |
| Mandelbrot, x86-64 | 264 ms | 175 ms | 1.51x |

Nightly only. https://www.mattkeeter.com/blog/2026-04-05-tailcall/

This is a real 1.5x-to-2x, and it is unavailable on stable Rust. It is also a
speedup **of a bytecode VM's dispatch loop**, so it only pays after a VM exists.

### Enum size, cloning, Box and Rc — measured on a Rust tree-walker

Nederlang: Monkey rewritten from C into Rust, kept as a **tree-walker**
throughout. `fib(35)`:

| Step | Time |
| --- | --- |
| Initial Rust tree-walker | 39.3 s |
| fxhash instead of SipHash | 32.3 s |
| Vec-based environments | 23.9 s |
| Pointer-based AST references | 4.2 s |
| String references | 3.7 s |
| Pointer tagging the value type | 2.1 s |
| Inlining the hot path | 1.8 s |
| The original C implementation | ~3.8 s |

Findings named: the `Object` enum was 32 bytes because of alignment padding; the
profiler showed **46.16 per cent of time** cloning `Object` and `Vec`; `Func`
variants owned two `Vec`s and forced clones.
https://www.dannyvankooten.com/blog/2022/rewriting-interpreter-rust/

**21.8x, on a tree-walker, with no bytecode anywhere.** And it ended up about 2x
faster than the C original. This is the strongest single datapoint in the report
for "the architecture was not the bottleneck; the representation was."

Two of those steps map onto work Vyrn has already done. "Vec-based environments"
is the frame change (`interpreter-loop-cost.md:88`). Vyrn's `fn expr` already
takes `&Expr` (`interp.rs:4410`), so the 23.9 s → 4.2 s step — the biggest one —
is already banked here. That is important: it means Vyrn's tree-walker is
already past the point where Nederlang's gains came from, which lowers the
remaining headroom estimate.

### Rust bytecode VM against C bytecode VM

Loxido (Rust clox) started "often several times slower than clox" and "even
slower than jlox". After optimisation it landed "between 20% and 50% more"
running time than clox. The steps, each with its own figure: raw pointers instead
of safe references, up to 74 per cent less time; aHash instead of SipHash, up to
45 per cent; fxhash, a further 25 per cent; a custom hash table, 44 per cent;
enum dispatch instead of trait objects, up to 28 per cent; unchecked stack ops,
25 per cent; pointer arithmetic for the program counter, 11 per cent. NaN boxing
in clox was worth only ±5 per cent.
https://ceronman.com/blog/my-experience-crafting-an-interpreter-with-rust/

Read the list. Almost none of those wins are "bytecode". They are hashing,
pointer chasing, dispatch shape and bounds checks — the same list as the
tree-walker's.

### A Rust tree-walker against a Rust VM of the same language, by one author, with published numbers

**NOT FOUND**. `rs-lox` holds both a tree interpreter and a bytecode VM in one
repository (https://github.com/lffg/rs-lox) but publishes no benchmark
comparison. This is the measurement the decision most wants and it does not
appear to exist.

---

## 7. The comptime angle

The question: the dominant use is compile-time evaluation. Each generator runs
once per build. A VM pays a compile-to-bytecode step per program; a tree-walker
does not. Does that change the answer?

Someone has weighed exactly this, in production, for exactly this use case.

Clang's constant interpreter has **two** back ends:

> "The compiler has two different backends: `ByteCodeEmitter` generates bytecode
> for functions, while `EvalEmitter` directly evaluates expressions during
> compilation without generating bytecode. Functions are compiled to bytecode,
> whereas top-level expressions in constant contexts skip bytecode generation."

And the reason:

> These expressions are "directly evaluated since the bytecode would never be
> reused", making this approach "equally efficient as the original evaluator" for
> single-use cases while improving performance on functions and loops.

https://clang.llvm.org/docs/ConstantInterpreter.html

That is the answer, from a shipping C++ compiler: **bytecode pays where code is
re-executed — function bodies and loops — and pays nothing where it runs once.**
So the trade is not "VM or tree-walker per program". It is "VM for the hot inner
bodies, direct evaluation for the one-shot outer expression."

The second precedent is Rust's own. Rust replaced an AST-walking constant folder
with an interpreter over **MIR**, the IR the compiler already had, rather than
inventing a bytecode. "The Rust compiler runs the MIR in the MIR interpreter
(miri), which sort of is a virtual machine using MIR as 'bytecode'" —
https://rustacean-station.org/transcript/oli-miri/ . The design reused existing
compiler infrastructure instead of adding a parallel representation —
https://github.com/rust-lang/miri/

A published measurement of bytecode-compilation overhead against total
compile-time-evaluation cost, for any language: **NOT FOUND**.

---

## What this says for Vyrn

**RECOMMENDATION, NOT A DECISION.**

### Ranked by measured payoff against risk

**1. Keep tuning the tree-walker. Do the slot resolution the census already
names.**

The census already names it: resolve each `Expr::Var` to a `(depth, index)` at
check time, and the 653 million probes go away
(`rfcs/census/interpreter-loop-cost.md:109`). Nederlang's "Vec-based
environments" step was 32.3 s to 23.9 s, and that was the *unresolved* version
of the same idea — https://www.dannyvankooten.com/blog/2022/rewriting-interpreter-rust/ .

Evidence this rung still has room: GoAWK's whole VM bought 13 per cent —
https://benhoyt.com/writings/goawk-compiler-vm/ . Loxido's biggest wins were
hashing and pointer chasing, not bytecode —
https://ceronman.com/blog/my-experience-crafting-an-interpreter-with-rust/ . And
`slice` is still 15.8 s of the 31 s step
(`rfcs/census/interpreter-loop-cost.md:115`) — one function, half the remaining
time, and it is not a dispatch problem at all.

Risk: lowest of the three. Parity risk is per-change and already bounded by the
40-program suite. This is the same kind of work that already produced 46.8 → 31.1.

Honest counterweight: Vyrn has already banked Nederlang's largest step (borrowed
AST nodes, `interp.rs:4410`), so do not expect 21.8x. Expect the census's own
estimate — "three length-compares per probe, which is much smaller than it was
when this arc started" (`rfcs/census/interpreter-loop-cost.md:113`).

**2. Closure compilation, and only if slot resolution is not enough.**

The one Rust measurement puts closures at 1.4 s against bytecode's 1.5 s and an
AST walk's 2.1 s — http://neilmitchell.blogspot.com/2020/04/writing-fast-interpreter.html .
On that evidence it takes the whole win at a fraction of the work, and it keeps
the AST shape, which keeps source positions, which keeps trap messages.

But read the rest of the record before committing. `starlark-rust` — the project
that produced that measurement — shipped closures, then shipped bytecode
eighteen months later
(https://raw.githubusercontent.com/facebook/starlark-rust/main/CHANGELOG.md).
They did not publish the delta. So the strongest pro-closure evidence comes from
a team that later moved past closures for reasons nobody wrote down.

Also note: closure compilation *includes* slot resolution. Option 1 is the first
half of option 2. Doing 1 first is not wasted work.

Risk: medium. It rebuilds the evaluation path in one go, so parity is proven
all-at-once rather than change-by-change — unless the RFC-0101 borrowed form is
used to migrate one node kind at a time
(`compiler/vyrn-lower/src/lib.rs:16`), which is exactly what that comment says
it is for.

**3. A bytecode VM. Not now.**

The published multiples — 4.5x for Monkey (https://compilerbook.com/), 6x for
Lox (https://benhoyt.com/writings/loxlox/), 4x for Ruby 1.9
(https://en.wikipedia.org/wiki/YARV) — all come from tree-walkers that had not
been tuned, or measurements that changed the host language at the same time.
Against a tuned tree-walker the figure is GoAWK's 13 per cent for 2,500 lines
(https://benhoyt.com/writings/goawk-compiler-vm/), and there is at least one case
where the VM came out 2–3x slower (https://dubroy.com/blog/two-little-interpreters/).

The cost side is specific and cited. Source positions need rebuilding — two
Python PEPs' worth (https://peps.python.org/pep-0626/,
https://peps.python.org/pep-0657/). Diagnostics move from "reject now" to
"reject later" and need an invalid-opcode mechanism
(https://developers.redhat.com/articles/2024/10/21/new-constant-expression-interpreter-clang-part-2).
And the closest analogue — replacing a compiler's compile-time evaluator while
keeping the old one as the reference — has run since July 2019 and is still
opt-in, with the two engines disagreeing on pointer comparison as recently as
bug 172165
(https://lists.llvm.org/pipermail/cfe-dev/2019-July/062799.html,
http://www.mail-archive.com/llvm-bugs@lists.llvm.org/msg94621.html).

A byte-identical trap message across three engines is a stricter oracle than
Clang's. That bug class is what would break it.

**Register versus stack, if a VM ever happens.** Register is worth about 32 per
cent with switch dispatch
(https://www.scss.tcd.ie/David.Gregg/papers/vee05-ShiGreggBeattyErtl.pdf) and 30
per cent on real Guile programs (https://wingolog.org/archives/2017/03/15/guile-2-2-omg),
and costs a register allocator (https://www.x-cmd.com/blog/251025/). Stable Rust
has only switch dispatch (https://pliniker.github.io/post/dispatchers/), which is
the row the 32 per cent figure sits in. Threaded dispatch would add 1.5x–2x on
top (https://www.mattkeeter.com/blog/2026-04-05-tailcall/) but needs nightly.

### Two things that change the shape of the question

**Do not build a bytecode. Vyrn already has an IR.** Rust did not invent a
bytecode for const evaluation; it interpreted MIR, the IR the compiler already
had (https://rustacean-station.org/transcript/oli-miri/). Vyrn has
`vyrn-lower`, already carrying a type and a line on every node, already
consumed by `vyrn-codegen`, already designed so an engine can migrate one arm at
a time (`compiler/vyrn-lower/src/lib.rs:16`). If the interpreter ever needs a
lower form, that is the form — and the parity argument gets *stronger*, not
weaker, because all three engines would then read the same lowering.

**The comptime angle changes the target, not the answer.** Clang's constant
interpreter compiles function bodies to bytecode and evaluates top-level
expressions directly, "since the bytecode would never be reused"
(https://clang.llvm.org/docs/ConstantInterpreter.html). Applied here: whatever
gets built should apply to loop bodies and function bodies, and never to a
one-shot generator's outer expression. That rules out any design that pays a
whole-program compile step per build.

### Is the answer "do neither"?

Not quite. "Do neither *yet*" is closer.

The specific work the repository already named — slot resolution, and the
`slice` decision — is cheaper than either architecture and is not finished
(`rfcs/census/interpreter-loop-cost.md:109`, `:115`). No cited case shows a
bytecode VM beating a *tuned* tree-walker by more than 18 per cent
(https://benhoyt.com/writings/goawk-compiler-vm/). Finish the cheap work, then
re-measure the 167 ns.

If after that the loop is still the CI bottleneck, the next step is closure
compilation over the RFC-0101 lowered form, migrated one node kind at a time
against the existing parity suite — not a bytecode VM.

### The gap that would change this recommendation

No source found isolates closures against bytecode, same language, same author,
published numbers, non-trivial workload. Section 2 rests on one small Rust
microbenchmark and one Elixir library. If that measurement existed and said
bytecode wins by 3x on a real workload, ranks 2 and 3 would swap. It does not
appear to exist. **NOT FOUND.**
