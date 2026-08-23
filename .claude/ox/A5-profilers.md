# A5 — What a Vyrn profiler has to be

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the one output file this job writes.

## Objective

Vyrn has no profiler. The owner wants one that works in Visual Studio Code and
that an agent can drive and read without a human looking at a flame graph. This
job collects what the five best profilers do, how they present it, and what
Vyrn already has to build on. It designs nothing.

## Part one — the five, one subagent each

- JProfiler, `https://www.ej-technologies.com/jprofiler`
- Intel VTune Profiler
- py-spy, `https://github.com/benfred/py-spy`
- JetBrains dotTrace
- pprof, `https://github.com/google/pprof`

For each, answer with citations:

| question | answer |
| --- | --- |
| Sampling, instrumenting, or both | |
| What it needs from the program being profiled | |
| Whether it can attach to a running process | |
| Overhead, as the vendor states it | |
| What it measures beyond wall time: allocation, lock contention, cache misses, system calls | |
| Its output file format, named exactly | |
| Whether that format is text or binary, and whether a program can read it without the tool | |
| How it presents a result: flame graph, call tree, timeline, table | |
| Editor or IDE integration, and how deep | |
| What it costs and what its licence is | |

Then two sentences: what this tool does better than the other four, and what it
does worse.

### The question that matters most

pprof is the important one for this job, because its format is a documented
protocol buffer and other programs read it. Record the `profile.proto` schema in
enough detail that a Vyrn emitter could target it: sample types, locations,
functions, mappings, and the string table. Cite the schema file.

## Part two — how an agent reads a profile

An agent cannot look at a flame graph. Collect what exists for machine-readable
profiling output:

- `pprof -top -text`, `-traces`, and the `-proto` output.
- py-spy `dump` and its `--json` flag.
- The speedscope file format, `https://github.com/jlfwong/speedscope`.
- Chrome DevTools `.cpuprofile` JSON.
- Firefox Profiler JSON.

For each: is the format documented, is it stable, how large is a typical file,
and could a language model read a hundred kilobytes of it and name the hot
function. Answer the last part by actually taking a sample file and trying.

## Part three — the Visual Studio Code side

Collect the extension points a profiler needs:

- How the JavaScript debugger extension presents a `.cpuprofile`.
- The `vscode.ProfileEditor` or equivalent custom editor API, if one exists.
- How the Rust and Go extensions expose profiling, if they do.
- What a CodeLens can show, since Vyrn already uses CodeLens. Read
  `compiler/vyrn-lsp/` and find where the existing lenses are produced, and cite
  the file and line.

## Part four — what Vyrn already has

Read and cite:

- `std/bench.vyrn` and the benchmarking RFC. What does `vyrn bench` already
  measure, and what does its JSON output contain?
- `compiler/vyrn-codegen/` — is there any instrumentation hook today?
- The interpreter — does it have a per-instruction or per-call counter anywhere?
- `rfcs/RFC-0063-ci-benchmarks.md` — what the CI already records.

Then a short list: what exists, and the smallest missing piece between it and a
call-tree profile.

## The output

One file: `rfcs/census/profilers.md`, with the four parts above, then:

- A section `Options for Vyrn`, giving three shapes a Vyrn profiler could take,
  each with what it would cost to build, what it would measure, and what it
  could not. Rank them. Mark the section `RECOMMENDATION, NOT A DECISION`.
- A section `The format question`, answering one thing plainly: should Vyrn emit
  pprof, speedscope, or its own format? Give the evidence for each. Do not
  choose.

## What this job must not do

- Do not build a profiler.
- Do not change the compiler.
- Do not write an RFC.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
