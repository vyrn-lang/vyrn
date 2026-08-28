# C1 — Where each benchmark loses, and to what

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

The benchmark corpus records a gap between Vyrn and the reference
implementations. Two gaps are already known and open: a `Map` operation about
211 times slower, and binary-trees using about 2.1 GB. This job attributes every
gap to a named cause with a measurement. It fixes nothing, because the fixes are
standard library and compiler decisions the owner will make.

Read `rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md` and
`rfcs/bench-0104/README.md` before starting. They explain the corpus, the
fixtures, and the harness.

## The programs

Eight programs are implemented: nbody, spectral-norm, fannkuch-redux,
binary-trees, fasta, reverse-complement, k-nucleotide, pidigits. Their sources
are under `examples/` and their probes under `rfcs/bench-0104/`.

Two are not implemented, regex-redux and mandelbrot. Those are job C2. Do not
touch them here.

## The method, per program

One subagent per program. A subagent must not spawn a subagent.

1. Build the release binary once: `cd compiler && cargo build --release -p vyrn-cli`.
2. Run the Vyrn program at the fixture size and at the game size. Record wall
   time and peak memory. Use `/usr/bin/time -v` where it exists, and the Windows
   equivalent otherwise. Say which you used.
3. Get a reference time. The reference sources are in `rfcs/bench-0104/ref/`.
   If a C reference is not runnable here, use the published Benchmarks Game
   number for the same N and say so, with the URL.
4. Compute the ratio. Report it as `Vyrn is N times the reference`.
5. Attribute the gap. This is the work. Bisect the program: comment out or
   shrink one phase at a time and re-measure, until you can say which loop or
   which call carries the time. Report the bisection steps and their numbers.
6. Name the cause in one of these classes, with a `path:LINE` in `std/` or in
   the compiler:
   - `ALGORITHM` — the Vyrn program does more work than the reference.
   - `STANDARD LIBRARY` — a `std/` function has the wrong complexity or
     allocates per call.
   - `CODE GENERATION` — the emitted code is worse than it needs to be. Show the
     emitted IR or the wasm for the hot loop.
   - `REPRESENTATION` — a value is boxed, copied, or reference counted where the
     reference uses a machine word.
   - `MISSING PRIMITIVE` — the language has no way to express the reference's
     approach. Name what is missing.
7. State the smallest change that would close the gap, and what it would cost.
   Mark it `RECOMMENDATION, NOT A DECISION`.

## The two known gaps get more

**The `Map` gap.** Find the exact operation that is 211 times slower and
reproduce it in isolation with a `bench` block. `MapVal` in the interpreter is
ordered pairs plus a hash index, so a linear scan is not the cause any more.
Find what is. Measure the interpreter, the native build, and the wasm build
separately, because the answer may differ between them. Report three numbers.

**The binary-trees memory.** 2.1 GB is the symptom. Find where it goes. Measure
allocation count and peak resident memory at increasing depths and give the
growth curve. Say whether the cause is the allocator, the ownership model
copying, reference counting, or the program itself.

## The output

One file: `rfcs/census/benchmark-gaps.md`.

1. One table: program, N, Vyrn time, reference time, ratio, peak memory, cause
   class.
2. One section per program with the bisection, the numbers, and the attribution.
3. `The two known gaps`, with the extra work above.
4. `Causes that repeat`, listing any cause class that explains three or more
   programs. That section is the most valuable one, because it names a single
   fix that moves several numbers.
5. `Ranked by seconds recovered per unit of work`. Mark it
   `RECOMMENDATION, NOT A DECISION`.

## What this job must not do

- Do not edit `examples/`, `std/`, or the compiler.
- Do not change a benchmark to make it faster. The corpus is a claim about a
  gap, and editing the program to hide the gap destroys the claim.
- Do not add a native body for anything.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
