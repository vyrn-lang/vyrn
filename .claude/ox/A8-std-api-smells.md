# A8 — Standard library calls that read wrong or invite a bug

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

The owner named two defects and expects more of the same kind.

The first: `joinWith(["ok \{ok}", admit(25), admit(5)], "\n")` should be a method
on the array, not a free function that takes the array first. Vyrn is
subject-first everywhere else. A collection is used as `sq.push(x)`,
`sq[j]`, `sq.length`. A free function that takes the subject as argument one
contradicts that.

The second: string operations work on bytes. A developer can miss that and ship
a bug. `s[i]` yields a `UInt8`. `byteLength` is bytes. A slice is a byte range.
Any of these applied to text that is not ASCII is wrong in a way that does not
fail loudly.

This job finds every instance of both, and every other call shape that reads
wrong. It changes nothing.

## Part one — every export, classified

Read every `export fn`, `export gen fn`, and every protocol method in all 38
modules under `std/`. One subagent per module, up to 32 at a time. A subagent
must not spawn a subagent.

One row per export:

| module | export | signature | subject | call form today | should be | class |

`subject` is the argument the operation is about, or `NONE` for a constructor or
a pure computation.

`class` is exactly one of:

- `SUBJECT FIRST` — already a method, or the subject is not an argument.
- `SUBJECT AS ARGUMENT` — the subject is argument one of a free function. This
  is the `joinWith` class. Give the method form it should have.
- `NO SUBJECT` — genuinely a free function. `random`, `now`, a parser entry
  point. Say why in one clause.
- `AMBIGUOUS` — two arguments could each be the subject. Name both.

Count each class per module and in total. The total for `SUBJECT AS ARGUMENT` is
the size of the problem, and the owner will want that number first.

## Part two — every byte-shaped string operation

Find every standard library function and every builtin that takes or returns a
byte position, a byte length, or a byte value, when the argument is a `String`.

One row each:

| operation | what unit it works in | what a caller would assume | what goes wrong on non-ASCII | is there a character-safe sibling | does the name say byte |

The last two columns are the finding. An operation that works in bytes, has no
character-safe sibling, and does not say `byte` in its name is the defect the
owner described. List those first, separately, under a heading `Silent byte
traps`.

For each silent byte trap, write the smallest program that shows the bug, run it
with `compiler/target/release/vyrn run`, and quote the actual output. Put the
programs in `C:\Users\demko\AppData\Local\Temp\claude\ox-a8\` and do not commit
them. A trap with a reproduction is evidence. A trap without one is a guess.

Then search the repository for callers that already have the bug. Search `std/`,
`site/`, and `examples/`. Every hit is a live defect and must be listed with
`path:LINE`. This part of the job may be the most valuable thing it produces.

## Part three — the other smells

While reading, record anything else in these classes, with `path:LINE`:

- Two exports that do the same thing under different names.
- An export whose name does not say what it returns, especially one returning
  `Option` or a `Result` shape without saying so.
- A pair where one is safe and one traps, and the trapping one has the shorter
  name.
- An argument order that differs between two functions that do similar things.
- A function taking three or more arguments of the same type, where a caller can
  swap two and still compile.

The last one is a compile-time-preventable defect and should be counted
separately.

## The output

One file: `rfcs/census/std-api-smells.md`.

1. Counts: exports read, `SUBJECT AS ARGUMENT` count, silent byte trap count,
   live defect count.
2. `Silent byte traps`, each with its reproduction and its output.
3. `Live defects in this repository`, each with `path:LINE`.
4. The full export classification table, by module.
5. `Other smells`, by class.
6. `What a fix would break`, which is the section that decides whether any of
   this is affordable: for the twenty largest `SUBJECT AS ARGUMENT` cases, count
   the call sites in this repository with `grep`, and give the number. Mark it
   `RECOMMENDATION, NOT A DECISION`.

## What this job must not do

- Do not rename anything.
- Do not add a method.
- Do not edit `std/`.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
