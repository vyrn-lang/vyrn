# std/cli.vyrn

Lines: 848. Exports: 10. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A caller declares a record type whose fields are command line options, passes the module name to the exported `gen fn cli(module)` (`std/cli.vyrn:735`), and gets back a generated module with `parse<Name>(argv) -> Validation<Name>` and `help<Name>() -> String` for every exported record type. The generated parsers call this module's runtime helpers: `readArgv`, `cliFlag`, `cliValue`, `cliIssues`, plus five `Issue` constructors and `wantsHelp`. Besides the 10 `export fn`/`export gen fn` declarations the module exports 3 record types (`CliOpt`:54, `CliHit`:63, `CliRead`:69). In-repo callers: `examples/clidemo.vyrn:14` and `examples/clifail.vyrn:9`.

## Findings

### 2. Algorithm complexity — LOW

What: `readArgv` resolves every option token through `cliOptAt`, a linear scan over the spec, so one argv walk is O(T x O) in tokens T and declared options O; the generated parser adds O(F x H), one full `cliValue` scan of hits H per value field F.
Where: `std/cli.vyrn:80-89` (the scan), called at `std/cli.vyrn:120` inside the token loop `std/cli.vyrn:104-147`; the emitted per-field `cliValue` call is built at `std/cli.vyrn:660`.
Evidence: bench scratch `C:/Users/demko/AppData/Local/Temp/claude/ox-a2/cli/b.vyrn`, run with `compiler/target/release/vyrn bench C:/Users/demko/AppData/Local/Temp/claude/ox-a2/cli/b.vyrn` from N:\lang. 250 pairs: 8 opts min 37.09 us, 64 opts min 72.27 us. 1000 pairs: 8 opts min 150.58 us, 64 opts min 278.57 us. Quadrupling tokens multiplies time by 4.06x (linear in T); raising opts 8x multiplies it by about 2 (the scan is linear in O, amortized against the per-hit copies). A second instance of the axis sits in comptime generation: `out = out + ...` inside loops at `std/cli.vyrn:595-598`, `std/cli.vyrn:694-707`, and `std/cli.vyrn:721-729` reallocates the whole accumulator each iteration, O(P^2 x L) bytes copied for P plans of line length L. Wall clock of that comptime cost: NOT MEASURED.
Cost if unfixed: every generated parser, today in `examples/clidemo.vyrn:38` and `examples/clifail.vyrn:16`, pays the scan per run; at realistic option counts the cost is microseconds.
Smallest fix: sort the spec once per process and binary-search `cliOptAt`, or key hits by field index instead of name; leave the comptime accumulation alone unless compile time ever shows up on a profile. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: `readArgv` copies nearly every string it touches even though it owns `argv`, and `cliIssues` deep-copies every `Issue` the walk just built.
Where: token copies at `std/cli.vyrn:108`, `std/cli.vyrn:125-127`, `std/cli.vyrn:141`, `std/cli.vyrn:144`; the redundant issue pass at `std/cli.vyrn:172-178` funnels through the three `.copy()` calls in `cliIssueOf` at `std/cli.vyrn:75-77`.
Evidence: one heap copy per free argument, two per valued hit, and one whole second `Array<Issue>` with three string copies per issue, by reading those lines; the generated parser opens with `cliIssues(r)` (`std/cli.vyrn:684`), so every parse pays it. Per-call timing split between copying and scanning: NOT MEASURED.
Cost if unfixed: every parse in `examples/clidemo.vyrn` and `examples/clifail.vyrn`; a few dozen small allocations per process start.
Smallest fix: move tokens out of `argv` and have the generated parser take `r.issues` directly instead of calling `cliIssues`. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30.
