# A6 — Prior art for six open language questions

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the output files this job writes.

## Objective

The owner has six open language questions. Each one is a design decision the
owner will make. This job supplies the evidence: what other languages did, what
it cost them, and what Vyrn already has that touches the question.

**Decide nothing.** Every file ends with options, not a choice. A file that
recommends one syntax and argues for it is a failed job. A file that gives four
designs with their costs is a passed job.

## Shared requirement for every topic

Every topic file has the same last two sections:

### What Vyrn has today

Read the repository, not the RFC titles. Cite `path:LINE`. State what exists,
what nearly exists, and what would have to change. Search `compiler/vyrn-frontend/`
for the parser and the checker, `compiler/vyrn-lower/` and
`compiler/vyrn-codegen/` for lowering, and `rfcs/README.md` for the RFC index.

### The options

Three to five designs. Each one gets:

| design | one-sentence description | what it costs in the parser | what it costs in the checker | what it costs in lowering | what breaks in existing code | who else does it |

Mark the section `RECOMMENDATION, NOT A DECISION`.

## The six topics, one subagent group each

### Topic 1 — lambda syntax

File: `rfcs/census/lang/lambda-syntax.md`.

Vyrn writes a lambda as `|x| x % 2 == 0`. See `site/guide/lambdas.vyrn`. The
owner wants something more canonical, in the shape Java uses.

Collect the syntax every relevant language uses, with an example of each: Java,
C#, Kotlin, Scala, Swift, Rust, Go, TypeScript, Python, Ruby, Elixir, OCaml,
Haskell, Zig, Nim, Gleam.

For each, record how it handles: no parameters, one parameter, several
parameters, a typed parameter, a block body, and a trailing lambda as the last
argument.

Then the part that matters: **the parsing conflicts.** For each candidate syntax,
say what it collides with in a language that also has Vyrn features. `->` collides
with a return type arrow. `=>` collides with a match arm. `(x) -> x` needs
arbitrary lookahead to tell a lambda from a parenthesised expression. Name the
conflict, say how the language that ships that syntax resolves it, and cite the
grammar.

Then count the work. Run these and put the numbers in the file:

```
grep -rn '|[a-zA-Z_]' --include=*.vyrn std/ examples/ site/ | wc -l
grep -rln '|[a-zA-Z_]' --include=*.vyrn . | wc -l
```

List every place the current syntax is produced or consumed: the lexer, the
parser, `vyrn fmt`, the LSP, syntax highlighting in `site/app/hl.vyrn` and in
the editor extension, the guide, and the documentation. Cite each.

### Topic 2 — unions of types with fixed property values

File: `rfcs/census/lang/literal-unions.md`.

The question, in the owner's words: can types be unions where a property is
fixed to a known value, so a check on that property tells the compiler which
member it is? And how does that behave with arrays?

Collect: TypeScript discriminated unions and literal types, Rust enums with
struct variants, Swift enums with associated values, Kotlin sealed classes, Java
sealed interfaces and pattern matching, Scala 3 union types, Flow, Python
`Literal` and `TypedDict`, Haskell GADTs, OCaml polymorphic variants, Zig tagged
unions.

For each, answer:

- Can a member be selected by testing one field?
- Is the test exhaustive-checked?
- Does the compiler narrow the type inside the branch?
- What happens to `Array<TheUnion>`? Is it covariant, invariant, or refused?
- Can a member be added without breaking every existing match?

The array question is the owner's, and it is the hard one. For every language,
say specifically what `Array<A | B>` means, whether an `Array<A>` can be passed
where `Array<A | B>` is wanted, and what goes wrong if it can.

Vyrn today: read how `enum` and its payloads work, how `match` narrows, and what
`Array` variance is. Cite lines.

### Topic 3 — coroutines

File: `rfcs/census/lang/coroutines.md`.

Reference the owner named: `https://github.com/Xudong-Huang/may`.

Collect: Go goroutines and the scheduler, Rust `async`/`await` with its state
machine lowering, the `may` crate and its stackful green threads, Kotlin
coroutines and suspend functions, Lua coroutines, Zig `async` and why it was
removed, Java virtual threads, C++20 coroutines, Erlang processes.

For each: stackful or stackless, how a stack is allocated and how it grows,
what the scheduler is, what colours a function, what the cancellation story is,
and what it costs at a call site that does not use it.

Then the specific questions for Vyrn:

- Vyrn has `spawn` and `join`. Read the concurrency RFC and
  `examples/concurrency.vyrn`. What is it today: an OS thread, a task, something
  else? Cite the implementation.
- Vyrn has three backends. A stackful coroutine needs a stack switch. What does
  wasm allow? Read about the wasm stack-switching proposal and say whether it is
  shipped anywhere. This constraint may rule out a whole family of designs, and
  saying so with a citation is the most useful thing this file can do.
- Vyrn has ownership and a move checker. What does a coroutine that is suspended
  across a `.await` do to a borrow? Look at how Rust answered this, and at what
  it cost.

### Topic 4 — attributes

File: `rfcs/census/lang/attributes.md`.

The question: Rust has attributes. Does Vyrn have anything like them, and what
should it have?

First, find what Vyrn already has. There is at least a `wasm-export-name`
attribute. Search the parser for every construct that decorates a declaration.
Cite every one. Report whether they share a grammar or are each special-cased.
That answer decides how much of this is a new feature and how much is
generalising something that exists.

Then collect: Rust attributes with their three forms, Java annotations with
retention policies, C# attributes, Python decorators, Go struct tags and build
tags, Zig comptime and `@` builtins, Swift property wrappers and macros.

For each: are they data or code, when are they read, can a user define one, can
they change what the code does or only annotate it, and how are they type-checked.

The key axis for Vyrn: Vyrn has `gen fn`, generators that run at compile time
and can read files. Read the generator RFC and `std/` for how a generator is
invoked. An attribute that runs a `gen fn` would be a Vyrn-shaped answer. Say
what would be needed for that, with citations, and what the sandbox rules would
have to be.

### Topic 5 — skipping checks that are already guaranteed

File: `rfcs/census/lang/refinement-subsumption.md`.

The question, in the owner's words: if one type forbids the characters `a` and
`b`, and another forbids only `a`, a value of the first satisfies the second.
Can the compiler know that and skip the check? What about other contracts, such
as a condition function?

This is refinement typing and subsumption. Collect:

- Liquid Haskell, and how it discharges a refinement with an SMT solver.
- F* and Dafny, and what they need from the user.
- ATS.
- Ada and SPARK subtype predicates, which is the closest thing to the owner's
  example that shipped in an industrial language.
- TypeScript template literal types and branded types, which get part of the way
  with no solver.
- Rust newtypes plus `TryFrom`, which is the zero-solver answer.
- Clojure spec and Elixir guards, for the run-time-only end.
- Refined types in Scala.

For each: what class of predicate can be expressed, is subsumption decided
automatically or asserted by the user, is a solver needed, what is the compile
time cost, and what happens when the solver cannot decide.

Then the decidability part, which the owner needs and which must not be fudged:
say plainly which predicate classes are decidable. Character-set exclusion is a
regular language question, and regular language containment is decidable. An
arbitrary condition function is not. Draw the line with citations, and say where
each language surveyed put it.

Vyrn today: it already has validated types and automatic validation at value
boundaries, plus `Validation<T>`, `Issue`, and `schemaOf`. Read those and cite
them. State exactly what a Vyrn validated type can express today, and whether
two of them can be compared at all right now.

### Topic 6 — operator overloading, and computing on a GPU

File: `rfcs/census/lang/operators-and-gpu.md`.

Three questions in one, because the owner asked them together.

**Operator overloading.** Collect: C++, Rust traits, Python dunder methods, Swift
with custom operators, Haskell, Scala, Kotlin, Julia, C#. For each: which
operators can be overloaded, can new operators be defined, how is precedence
decided, is dispatch static or dynamic, and what stops a library from making
code unreadable.

The owner asked how to beat Python. Say concretely what Python pays: dynamic
dispatch through `__add__` and `__radd__`, boxing every intermediate, and no
fusion. Then say what a statically dispatched, monomorphized language gets for
free. Vyrn protocols are already static and monomorphized. Cite that, and say
whether operator overloading is just a protocol with syntax.

**Array and GPU computing.** Collect: NumPy broadcasting and its ufunc protocol,
Julia broadcast fusion and why it composes, JAX tracing and `jit`, CUDA and HIP
programming models, and what a kernel needs from a language: contiguous memory,
no allocation, no dynamic dispatch, a known thread index.

**HVM4**, `https://github.com/HigherOrderCO/HVM4`. Read the repository. Describe
its model honestly: what it evaluates, what parallelism it claims, what the
claims are measured against, and where it is on the path from research to use.

Then Vyrn: it has SIMD types today, `F32x4` and `I32x4`. Cite the SIMD RFC and
the implementation. Say what would have to be true for a Vyrn function to run as
a GPU kernel, and which of those things Vyrn already has. Note the standing
constraint: no backend-specific standard library implementations.

## The output

Six files under `rfcs/census/lang/`, named above, plus
`rfcs/census/lang/README.md` with one table: topic, how big the change would be,
what it would break, and whether the census found a design that fits Vyrn
without a new solver or a new backend.

## What this job must not do

- Do not change the lexer, the parser, the checker, or any `.vyrn` file.
- Do not write an RFC.
- Do not pick a syntax.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
