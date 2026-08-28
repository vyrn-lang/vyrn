# std/vyx-hints.vyrn

Lines: 894. Exports: 3. Read at commit 82234d6a01922072cd24c289cf49ed7c2d592c09.

## What this module is for

A project imports `vyxHints(dir)` or `vyxHintsConfigured(dir, config)` at compile time. The generator reads every `.vyx` file under `dir`, parses each with `std/vyx`'s `vyxParseTemplate`, applies eleven accessibility, security and performance rules to the parsed tree, and returns the reports as `//@diag` lines plus one trivial declaration (`std/vyx-hints.vyrn:68-96`, `:185-194`). `std/hints` filters the reports by project policy and waivers. All three exports are `gen fn`s, so the work happens during generation, not at run time. Directory listings come from the sorted builtin (`listDir` sorts for determinism — `compiler/vyrn-cli/src/remote.rs:40`, `compiler/vyrn-frontend/src/interp.rs:5263`), so report order is stable across runs and platforms.

## Findings

### 2. Algorithm complexity — MEDIUM

What: Report accumulation copies the whole grown output once per report, so checking a file that earns R reports does O(R²) byte copies.

Where: `std/vyx-hints.vyrn:207` (the proving loop `out = out + vhNode(...)`; the same shape at `:140`, `:142`, `:147`, `:249`, `:299-496`).

Evidence: Timing method: `vyrn bench` cannot time a `gen fn` (the native backend fails the link with `use of undefined value '@vyrn_vhCheck'`), so each number is wall clock of `compiler/target/release/vyrn run <scratch driver>` under the interpreter, where the driver repeats one identical `vhCheck` call r times; per-call cost = (wall clock − 67 ms process start) / r. A clean 256-element template costs 193 ms/call; parsing alone through `vyxParseTemplate` costs 187 ms/call; a 256-`<img>` template that fires 512 reports costs 2400 ms/call (14.469 s wall for 6 calls). The fired-rule cost above parsing grows ×2.5, ×3.2, ×6.0 across doublings of report count R = 64 → 128 → 256 → 512; linear work grows ×2, so the excess is the quadratic copy. On a tree that fires nothing the walker adds little over the parse (8.3 ms against 7.0 ms per call at 32 elements). Interpreter numbers; native numbers NOT MEASURED.

Cost if unfixed: Any project that imports `vyxHints` pays this at every build, and a tree with many fired hints pays quadratically in the hint count; today no in-repo site or example imports the module, which caps this at MEDIUM.

Smallest fix: Accumulate reports in an `Array<String>` and join once at the top, threading one buffer through `vhNodes`, `vhElem` and `vhScan`. RECOMMENDATION, NOT A DECISION.

### 26. Syscall frequency — LOW

What: Deciding whether an entry is a directory performs a full `listDir` — a complete directory read, allocation and sort — and each real subdirectory is then listed a second time by the recursion.

Where: `std/vyx-hints.vyrn:157-162` (`vhIsDir` answers a boolean by enumerating everything), reached from `:145`; the recursive re-listing sits at `:146`.

Evidence: Same timing method as above, over three scratch trees of 300 entries. A tree of 300 `.txt` files costs 17.8 ms per `vyxHints` call (1 root `listDir` + 300 probe `listDir`s, zero components read, nothing checked). A flat tree of 300 clean `.vyx` files costs 105 ms (1 `listDir` + 300 `readFile`s). A tree of 300 one-file subdirectories costs 132 ms (601 `listDir`s + 300 `readFile`s), so the probe-plus-rescan pattern adds about 45 µs per directory here. Interpreter numbers; native syscall counts NOT MEASURED.

Cost if unfixed: Every build that imports `vyxHints` over a mixed tree performs one wasted full directory enumeration per non-component file, and twice per real directory.

Smallest fix: List each directory once, classify entries from the listing the platform already returned, and recurse on those entries without a second `listDir`. RECOMMENDATION, NOT A DECISION.

### 8. Allocation frequency — LOW

What: The hot paths allocate small strings that a bound-carrying formulation would not need.

Where: `std/vyx-hints.vyrn:684` (`a.value.copy()` on every `vhValue` lookup, which `vhNeedsAlt` at `:509` and `vhIsNameable` at `:530`, `:533` call per element), `:561` (`toLower` over the whole URL value when only the scheme prefix matters), `:659` (`trim` builds a fresh copy per `tabindex` value), `:133` (`dir + "/" + name` is two concatenations per entry).

Evidence: Per-site magnitudes NOT MEASURED; they sit inside the measured totals above (for scale, the whole 300-file flat-tree scan costs 105 ms per call, so no single site dominates).

Cost if unfixed: Generation-time garbage rises with element and attribute counts on every build that runs the checker; the checker itself pays it, no runtime caller does.

Smallest fix: Compare and slice scheme prefixes in place instead of lowering the whole value first, and return borrowed views or compare without copying in the attribute lookups. RECOMMENDATION, NOT A DECISION.

### 24. Branch predictability — LOW

What: The inline-handler predicate is a chain of up to 59 literal string comparisons per attribute, all on the error-severity path of every attribute of every element.

Where: `std/vyx-hints.vyrn:613-650` (25 pointer handlers at `:618-628`, 7 keyboard at `:630-634`, 11 form at `:636-641`, 16 load at `:643-650`), called from `:298`.

Evidence: Comparison count comes from counting the listed literals; loop bounds prove the linear scan. Cost per attribute is NOT MEASURED separately; it is bounded by the fired-versus-clean gap above, and attributes per element are few.

Cost if unfixed: Templates with many attributes pay dozens of failed comparisons each; correctness is unaffected because the named-list design at `:606-612` is deliberate.

Smallest fix: Dispatch on the first two bytes (`on`) before the named-list test, keeping the exact-name match as the only accept condition. RECOMMENDATION, NOT A DECISION.

## No finding

No finding: 1, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14, 16, 17, 18, 19, 20, 21, 22, 23, 25, 27, 28, 29, 30.
