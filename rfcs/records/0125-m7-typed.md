#### Every rule of the checker's two walks, witnessed by one program (2026-09-25, `m7-typed`)
RFC-0125, milestone M7.
Decision: count before moving. `Checker::stmt` and `Checker::expr` state 60 refusal sites today, and one program witnesses all 60. The lead's brief (`m7-typed`).
Went: nothing. Stayed: all 60, because moving one needs the decision below.
Lines: `checker_census.rs` 772 to 792. `tests/checker-rules.vyrn` is new: 65 functions, one refused statement in each of 60, one body with two refused statements from two groups, and a typed-looking caller of a refused generic function. Refusals: 0 lost / 0 gained. Manifest: untouched. The corpus gains one refused root, this program.
Licence:
- `tests/pins/checker-rules.tsv`: what `vyrn check` prints over the program, 64 diagnostics. The checker stops at a statement's first error and goes on to the next statement, so each sentence prints once. A throwaway build tagged each message with the `cerr!` site that made it (`[site N]`), and each of the 60 sites answered exactly one line.
- the same build over the 492 corpus roots: 2 of the 60 sites are reached, the assign to an immutable binding (`examples/a6_reassign`) and the `drop` of a type parameter (`examples/dropparam`).
- `cargo nextest run --release -p vyrn-cli`: 670 passed. `letswalk`, `lowered_dump`, `columns` and `kernel` `--ignored` passed.
Findings:
- track-er counted 63 sites and 3 `Recorded` writes on 2026-09-11. The 3 writes are the judgment's record, not refusals. Which 3 sites left since then was not traced.
- typed.rs states none of the 60. The kernel states `break` and `continue` outside a loop in other words ("a `break` outside a loop").
- the sites by what they read: a store needs `mut` (3), `break`/`continue` (2), `drop` (4), a literal against its slot (8), a missing-`length` hint (1), an unknown name (5), a failed inference (6), and a check on computed types (31: mismatches, conditions, operands, fields, keys, containers, a missing field, a `where` record field).
- `vyrn check` builds the core only when the checker gave no diagnostic (`vyrn-frontend` `lib.rs`, before `movecheck::refusals`). So a rule stated over the core disappears from a file that has any other checker error, and it uncovers kernel refusals that the checker's error hid. Two witnesses show it: an immutable assign beside `if 1` prints 2 lines today; an immutable assign beside a use after `consume` prints 1.
- `testsweep --ignored` fails on main 513bae71: `refusals.rs` literal #937 is accepted without the kernel and refused with it (`kids` handed to a `consume` parameter). This track does not touch it.
- the checker states every refused statement of a body, not only the first (`Checker::block`). So under decision A a body with a checker refusal and a moved rule's refusal loses the second: `twoPasses` in the census program prints lines 299 and 300 today, and would print 300 alone.
Left: every move, blocked by stage 2 of decision A (#474): the core built for each body the checker typed.

#### The core is built for every body the checker typed (2026-09-25, `m7-typed`)
RFC-0125, milestone M7, decision A stage 2 (#474).
Decision: a program the checker refused in function bodies alone still has the core built for the rest, and the kernel's list is still read only when no pass refused. The lead's staging.
Went: nothing. Stayed: every rule; this stage moves none.
Lines: `vyrn-frontend` `lib.rs` 263 to 351 (+88, `lower_typed`), `checker.rs` 14,962 to 14,988 (+26, the refused set). Nothing is deleted: this is the pipeline the rules leave the checker through. Refusals: 0 lost / 0 gained. Manifest: untouched.
Shape: `check_accum_with_json_types` answers the functions with a refused statement, or `None` where a refusal stands outside a function body. `lower_typed` moves the refused functions out of the program by value, and every function that calls or names one, to a fixpoint. It builds nothing when a test, a bench, module state or an impl method names one, and nothing for a generator's own program. It runs `own::analyze`, drops the kernel's refusals, and moves the functions back in the source's order. No line of `vyrn-lower` changed, so it composes with `m7-slotrel`'s fixpoint in `augment`: a refused body is never in the program the lowering sees.
Licence:
- `vyrn check` over 496 roots on af4c7316, the census commit's binary against this one: byte-identical stderr and exit codes, 419 accepted, 77 refused. 18 refused roots take the new build, and the same 18 under a debug build print the same bytes.
- `checker-rules` pin unchanged. `nextest -p vyrn-cli` 670 passed; `lowered_dump`, `kernel`, `wasmhash` (check), `letswalk`, `columns`, `refusals` `--ignored` passed; `vyrn-frontend` 1,084 passed; `vyrn-lsp` 100 passed.
- pins: `checker-census` shared machinery 2,371 to 2,397 lines; the anchor of `check_accum_with_json_types` follows its new signature.
- time, `vyrn check`, best of interleaved runs: `site/export.vyrn`, accepted, 1,529 to 1,582 ms (8 rounds; medians 2,279 and 2,254, inside the band). The same file with one refused function added: 679 to 1,572 ms (6 rounds), because the core is now built for it; that is the accepted program's cost. The census program: 182 to 313 ms.
Findings:
- the editor pays the same: `load_warned` reaches `check_and_synthesize`, so a keystroke in a file with a checker refusal now builds the core.
- a typed caller of a refused generic function is moved out with it, the lead's fallback, not an invented instance. `callsRefused` in the census program is the witness: once the store rule is the judgment's, its line goes.
Left: the rules, groups 1 to 4 first.

#### The judgment reaches every body the checker types (2026-09-25, `m7-typed`)
RFC-0125, milestone M7, the reach decision (#487).
Decision: a gap in a typed body is an internal error, and a generic function with no instance is built once for the judgment. The lead's, on the first move's witnesses.
Went: `init_ty`, the builder's second statement of a global initializer's type; `named_place` reads the checker's recorded type instead. The silent `not lowered` trace line. Stayed: nothing.
Lines: `core.rs` 9,223 to 9,230, `vyrn-lower` `lib.rs` 1,495 to 1,538 (`uninstantiated`). Refusals: 0 lost / 0 gained over the corpus. Manifest: untouched.
Shape: `refuse_gap` reports every gap. A gap with a rule is a refusal, as before. A gap without one says "internal error: the core cannot state ..., so `f` is not judged". A generic body with no instance is walked with each parameter standing for itself, then built and judged. It places no row and is never emitted.
Licence:
- `vyrn check` over 503 roots, main b640e67d against each commit: byte-identical stderr and exit codes, 426 accepted, 77 refused, 0 internal errors.
- one gap outside the corpus: `vyrn-frontend` `semantics::module_state_of_fn_type_with_init_order`, a call through module state of function type (`cur(10)`). Closed at its home: the value is read out of the global and called through, as a forced `lazy` field is. The body now takes the core's walk; its wasm differs from main's and prints 31 under both.
- `c20` (untyped module state, `arr[0] = 4`) is refused by main and by this tree at 3:0.
Findings:
- the lead asked for each parameter as an opaque OWNED type. `declared::owns_heap` answers `false` for `Type::Param`, and making it `true` changes every generic analysis in `own.rs`. The store rule reads no ownership, so the parameter stands as written. The kernel does not judge these bodies.
- `site/export.vyrn`, best of 8 interleaved: 4,431 ms on main, 4,373 ms at the tip. The runs spread from 4.4 s to 11.8 s, so the noise band is wider than the difference.

#### A store needs `mut` is stated once, over the core (2026-09-25, `m7-typed`)
RFC-0125, milestone M7, decision A, group 1.
Decision: `NameInfo.mutable` holds the fact; the typed judgment reads the root of every store. The lead's representation.
Went: the checker's three sites (assign, field, index) and its six unit tests of them. Stayed: the `remove` and `pop` refusals in `Checker::expr`, which are group 2's.
Lines: `checker.rs` 14,988 to 14,911. `typed.rs` 906 to 998 (`stores`). `core.rs` 9,230 to 9,296 (the slot and `mutable`). `refusals.rs` 3,103 to 3,166. Refusals: 0 lost / 0 gained over the corpus. Manifest: untouched.
Shape: a `let mut` or a `modify` parameter is mutable; binders, `for` variables and lambda parameters never are. A store is a `St::Store` or an `Arg::Place` passed to `modify` (a removal through a path). Module state answers through `GlobalDecl.mutable`. One refusal per statement site, across every instance of a generic. The refusals reach `vyrn check` through `own::typed_refusals`, and a typed refusal silences the kernel's list.
Licence:
- `vyrn check` over 503 roots against main b640e67d: byte-identical except the census program. 0 corpus bodies changed output.
- `checker-rules` pin: 299 and 314 go (decision A), six witnesses come: `h.a[i] = 5`, `k["a"] = 2`, `h.a.pop()`, untyped module state, an uncalled generic, and at 353 the checker's "`x` is Int64 but assigned String" in place of the moved rule's sentence (#487).
- 23 witness programs, main's binary against the tip: equal except that last one. They cover two stores on one line, a generic called twice, tests, benches, impl methods, lambdas, binders, parameters and an imported module.
- `nextest -p vyrn-cli` 672 passed. `-p vyrn-frontend -p vyrn-lower` 1,081 passed. `kernel`, `effects`, `typed`, `coretables`, `coredrive`, `wasmhash` (check), `lowered_dump`, `refusals`, `letswalk` and `columns` all passed under `--ignored`. `vyrn-lsp` 100 passed.
- pins: `checker-census` typing judgment 132 to 129 refusals; `forms` `Stmt::Let` in lower 15 to 16; `frontend-census` movecheck 891 to 901.
- coredrive: the emitter took the core's walk for 21,033 of 21,172 bodies; 1 of 168 programs emit the same module, 167 run the same.
Time: about 150 minutes: 70 work, 60 gates, 20 builds.
Left: groups 2 to 4.

#### A loop exit and a written `drop` are stated once, over the core (2026-09-25, `m7-typed`)
RFC-0125, milestone M7, decision A, groups 2 and 3.
Decision: the judgment states both. The kernel's copy of the loop rule goes, and the kernel ends the path there. The lead's.
Went: group 2, the checker's two sites, its `in_loop` flag (8 lines of save and restore), the kernel's two refusals and one checker unit test. Group 3, the checker's four `drop` sites, `resolves_to_global` and one unit test.
Stayed: group 4, the literal checks, because the core cannot state them (Left).
Lines: `checker.rs` 14,911 to 14,862 (group 2) to 14,765 (group 3). `typed.rs` 998 to 1,042 to 1,127. `core.rs` 9,302 to 9,313 to 9,327. `kernel.rs` 3,084 to 3,088. Refusals: 0 lost / 0 gained over the corpus. Manifest: untouched.
Shape: `St::Break` and `St::Continue` carry their line. `typed::loops` refuses one with no `St::Loop` around it in its own frame, so a lambda does not inherit the loop. `Body.unbound_drops` holds a `drop` whose name no binding answers. `typed::drops` words it as module state or an unbound name, and refuses a bound name whose type owns no heap or is a type parameter. The types must be as the checker typed them, so every generic function is built once with its parameters as written, not only one with no instance. An instance is not judged for `drop`.
Licence:
- `vyrn check` over 507 roots against main 6ad6dfc2: byte-identical, 430 accepted, 77 refused, 0 internal errors. The census program under main's binary and the tip: byte-identical.
- 27 witness programs (C:/wtboxtmp/w2, w3), main against the tip: equal except the three decided cases. Two print the checker's line alone, because the checker refuses their body. One (`drop x`, `x = 2`, `break` in one body) prints the moved `mut` line as well: the checker no longer refuses that body.
- `checker-rules` pin: 361 (`break` in a lambda in a loop) and 369 (`drop` of `T` in a generic with an instance) are new witnesses.
- `nextest -p vyrn-cli` 674 passed. `-p vyrn-frontend -p vyrn-lower` 1,079 passed. The ten ignored suites passed, with `wasmhash` in check. `vyrn-lsp` 100 passed.
- pins: `checker-census` typing judgment 129 to 127 to 123 refusals; `declarations` and `surface` lose the checker's mentions.
- coredrive: 21,053 of 21,172 bodies take the core's walk; 1 of 168 programs emit the same module, 167 run the same.
- `site/export.vyrn`, best of 8 interleaved: 3,835 ms on main, 3,485 ms at the tip, inside the noise.
Time: about 110 minutes: 50 work, 45 gates, 15 builds.
Left: group 4, blocked by a decision. A literal row names no width and no node (`Lit`, "the WIDTH is not here"). So the judgment cannot tell `let x: UInt8 = 300` from `let x: Int64 = 300`, and cannot say which statement's line it is on. The checker states the fit rule twice (`Checker::expr` and `adapt_int_literal`).
