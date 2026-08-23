# A2 — The standard library against thirty quality axes

Read `.claude/ox/RULES.md` first.

Working directory: `N:\lang`, branch `main`. Read-only on the repository except
for the output files this job writes.

## Objective

Read every module in `std/` and record, with evidence, where it is slow, where
it allocates, where it is not deterministic, and where it is unsafe to call
twice. The owner will decide what to fix. This job finds and measures. It fixes
nothing.

## The thirty axes

1. Cache locality
2. Algorithm complexity
3. Side effects (the target is zero)
4. SIMD use
5. SWAR use (SIMD within a register)
6. Lock-free structure
7. Peak memory use
8. Allocation frequency
9. Disk and network input and output
10. Control flow predictability
11. Pipeline stalls
12. Amdahl's law limits
13. Lock contention
14. Task independence
15. Best, worst and average case
16. Adaptive behaviour
17. Numerical stability
18. Precision loss
19. False sharing
20. Thread safety
21. Footprint size
22. Vectorization
23. Instruction-level parallelism
24. Branch predictability
25. Data dependency chains
26. System call frequency
27. Page fault rate
28. Initialization overhead
29. Teardown cost
30. Determinism

## The honesty rule, which matters more than coverage

Most axes do not apply to most modules. A module written in Vyrn does not
control page faults. A pure text function has no lock contention.

For each module, report ONLY the axes where you found something concrete, and
each finding must carry a file and line. Put every other axis on one line at the
end of the file: `No finding: 4, 6, 11, 13, 19, ...`.

A file that scores all thirty axes with prose is a failed job. A file with four
findings, each with a line number and a measured number, is a passed job.

Never write "could be optimized with SIMD" unless you name the loop, its line,
and the measured time it takes now.

## Measurement

The repository has a benchmarking facility. Use it.

- `bench` blocks and `blackBox` are described in `rfcs/RFC-0055-benchmarking.md`.
- `compiler/target/release/vyrn bench <file>` runs them.
- Build the binary first: `cd compiler && cargo build --release -p vyrn-cli`.

For any claim that something is slow, write a small `bench` block in a scratch
file under `C:\Users\demko\AppData\Local\Temp\claude\ox-a2\`, run it, and quote
the number. Do not add bench blocks to `std/`. Do not commit the scratch files.

For any complexity claim, state it as `O(...)` in terms of named inputs, and
point at the loop that proves it.

## The fan-out

There are 38 modules in `std/`. Give one subagent one module. Run at most 32 at
a time, then run the rest. A subagent must not spawn a subagent.

Order the batches so the largest modules start first:
`vyx.vyrn`, `ui.vyrn`, `graphql.vyrn`, `http.vyrn`, `rpc.vyrn`, `i18n.vyrn`,
`von.vyrn`, `tw.vyrn`, `icons.vyrn`, `vyx-hints.vyrn`, `num.vyrn`, `html.vyrn`,
`cli.vyrn`, `jsonread.vyrn`, `contract.vyrn`, `text.vyrn`, `strings.vyrn`,
`jsondec.vyrn`, `json.vyrn`, `strpred.vyrn`, `codecs.vyrn`, `openapi.vyrn`,
`scan.vyrn`, `stream.vyrn`, `slots.vyrn`, `connect.vyrn`, `bench.vyrn`,
`args.vyrn`, `hints.vyrn`, `hash.vyrn`, `random.vyrn`, `symbolmap.vyrn`,
`time.vyrn`, `storage.vyrn`, `math.vyrn`, `diag.vyrn`, `arrays.vyrn`,
`fallible.vyrn`.

## The per-module output

One file per module: `rfcs/census/std-quality/<module>.md`, where `<module>` is
the file name without `.vyrn`.

Structure:

```
# std/<module>.vyrn

Lines: N. Exports: N. Read at commit <sha>.

## What this module is for

One paragraph. What a caller uses it for.

## Findings

### <axis number>. <axis name> — <severity>

What: one sentence.
Where: `std/<module>.vyrn:LINE`.
Evidence: the measured number and the command that produced it, or the loop
bounds that prove the complexity.
Cost if unfixed: one sentence naming the caller that pays.
Smallest fix: one sentence. Mark it `RECOMMENDATION, NOT A DECISION`.

## No finding

No finding: <comma-separated axis numbers>.
```

`severity` is one of `HIGH`, `MEDIUM`, `LOW`. `HIGH` means a caller in this
repository pays a measured cost today. Name the caller.

## The rollup

After every module file exists, write `rfcs/census/std-quality/README.md`:

1. One table, one row per module: lines, exports, count of `HIGH`, `MEDIUM`,
   `LOW`.
2. A section `The twenty findings worth fixing first`, ranked by measured cost,
   each linking to its module file. Mark it `RECOMMENDATION, NOT A DECISION`.
3. A section `Patterns that repeat`, listing any finding that appeared in three
   or more modules. This section is the most valuable part of the job. A defect
   in three modules is a standard library design problem, not a module problem.

## What this job must not do

- Do not edit any file in `std/`.
- Do not edit the compiler.
- Do not add a native body for anything. See `.claude/ox/RULES.md`.
- Do not commit and do not open a pull request. Write the output file or
  files, then stop. The repository owner commits them. A commit from this job
  will collide with the other jobs running beside it.
