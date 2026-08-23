# C2 — The two benchmarks Vyrn cannot write, and what would unblock them

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the two output files this job writes.

## The answer is already known. Confirm it, then cost it.

`rfcs/RFC-0104-a-benchmark-is-a-claim-about-a-gap.md`, near line 407, records why
two Benchmarks Game programs have a committed fixture and no Vyrn program beside
them. Read that passage first and quote it in both output files.

**regex-redux.** Vyrn's `=~` is an anchored full match against a pattern that
must be a compile-time constant. It answers neither how many nor where, and
there is no replacement by pattern. regex-redux needs a regular expression that
searches, counts, and replaces, over a pattern chosen at run time.

**mandelbrot.** The pixels are correct and cannot leave the program. `print` and
`writeFile` both take a `String`, and `stringFromBytes` refuses a packed row.
mandelbrot needs a way to write bytes to a file and to standard output.

Confirm both by reading the current implementation. Cite the lines in the
compiler and in `std/` that make each statement true. If either statement has
stopped being true since the RFC was written, say so. That is the first useful
thing this job can produce.

## Part one — the byte sink

File: `rfcs/census/blocked-byte-sink.md`.

This is the small one. Establish, with citations:

- Every way a Vyrn program can send data out today. Read the input and output
  RFC and `std/`. List each with its signature.
- Where the `String` requirement is enforced. Name the function in the checker
  and in each backend.
- What `stringFromBytes` refuses and why. Quote the rule.
- What the three backends can each actually do. The interpreter has a real file
  system. Native has one. wasm has WASI preview 1 with a shim. Say what each
  supports for writing arbitrary bytes to a file and to standard output, and
  what the browser shim in `web/` does when there is no file system.

Then give three designs, each with the full cost:

| design | the API | checker changes | interpreter changes | native changes | wasm changes | browser behaviour | parity risk |

Candidates to cover, and add any others you find: a `writeFileBytes` taking
`Array<UInt8>`; a `Stream<UInt8>` sink built on the existing linear stream type;
and a general output handle that `print` and `writeFile` both become uses of.

The parity column is the one that decides this. All three backends must produce
identical bytes. Say for each design where that could break, especially the
newline translation on Windows that the benchmark harness already has to
normalise.

Mark the section `RECOMMENDATION, NOT A DECISION`.

## Part two — the regular expression engine

File: `rfcs/census/blocked-regex.md`.

Establish, with citations:

- Exactly what `=~` supports today. Read the implementation. The pattern is a
  compile-time constant and is compiled to a DFA. Find that code and cite it.
  Say what syntax it accepts, what it rejects, and what the match returns.
- Whether the existing DFA compiler could be reused for a searching engine, or
  whether it is built around the anchored full-match assumption. This is the
  central technical question of the file.
- What regex-redux actually needs. Read the Benchmarks Game specification for
  it. It needs counted matches of several patterns and a chain of replacements.
  List the exact patterns and operations.

Then survey the prior art, one subagent each, with citations:

- Rust `regex`, and its guarantee of linear time without backtracking.
- RE2 and its automaton approach.
- The Thompson construction and the Pike virtual machine.
- Hyperscan, for the many-patterns-at-once case that regex-redux is.
- Go `regexp`.
- Why backtracking engines have the catastrophic case, with one concrete example
  that a Vyrn user could hit.

For each: what it guarantees, what syntax it gives up to guarantee it, how big
the implementation is in lines, and whether it needs anything the Vyrn language
does not have.

Then the Vyrn-specific question, which is the one the owner will decide on:

**Where does it live?** Three options, and cost each:

1. In `std/`, written in Vyrn, portable across all three backends by
   construction. State the expected speed given the measured interpreted
   throughput, which the string census recorded at about 1.5 MB per second for
   scanning. Say plainly whether a Vyrn-hosted engine can run regex-redux at the
   game size in a reasonable time, with the arithmetic.
2. As a compile-time generator. A `gen fn` compiles a pattern to Vyrn code.
   Vyrn already has generators, and a pattern known at compile time is the
   common case. State what this cannot do, which is a pattern chosen at run
   time, and whether regex-redux needs that.
3. As a compiler builtin. **Cost this, then note that it violates the standing
   rule in `.claude/ox/RULES.md` about backend implementations.** Include it so
   the owner sees the trade, not because it is available.

Mark the section `RECOMMENDATION, NOT A DECISION`.

## What this job must not do

- Do not implement either thing.
- Do not write an RFC.
- Do not add a benchmark program.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
