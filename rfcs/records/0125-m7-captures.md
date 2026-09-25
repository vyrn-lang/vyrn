#### A lambda captures the binding in scope, not every name spelled the same (2026-09-25, `m7-captures`)
RFC-0125, milestone M7.
Decision: `core::Builder::captures` walked every scope entry and captured each one whose source name the lambda mentions, so a lambda under a shadowing `let` captured the shadowed name too (#483). The kernel judged that capture as a read and refused a valid program once the outer name had moved. A scope entry is captured only when `Builder::lookup` resolves its spelling to it, the lookup the body itself uses. The lead's brief; the rule is `captures`' alone.
Went: the shadowed capture. The row for the shape `a-lambda-that-captures-a-shadowing-name` is `closure(s, x)` with the inner `x` alone, so `Fn_::core_lambda` takes its `let`: `main` has one `memory.copy` fewer (2 to 1) and a frame of 16 bytes instead of 32, and under `VYRN_FORM_TALLY` no statement of `main` reaches the arm (on main, its `if` and the lambda's `let` did).
Lines: `core.rs` 5 added, 1 removed. Refusals: 1 lost (the false one, #483) / 0 gained. Manifest: untouched.
Licence:
- #483's program: main's binary refuses it ("`x` is used here but was already consumed by `keep(..)` on line 7", exit 1) on both walks; the tip prints 25, exit 0, no leak line, on both walks under `VYRN_LEAK_CHECK=1`. It is the shape `a-lambda-that-captures-a-shadowing-name-after-the-outer-one-moved`, 0 break, 0 continue.
- the old shape prints 3221 on main and at the tip, with and without `VYRN_NO_CORE_WALK`, under `VYRN_LEAK_CHECK=1`.
- `kernel --ignored`: 27,061 accepted, 0 refused, 0 unlowered, as on main. `effects`: 29,619 judged, 9,945 pure. `typed`: 236,909 stores judged, 0 unjudged.
- `coredrive --ignored` with the manifest check: taken 21,033 of 21,172 on b640e67d with and without the fix, and 21,053 on c2f59d6f (#488) with and without it (no corpus program has a lambda under a shadowing `let` that the old row sent to the arm); 0 run apart; the manifest check is green with no row moved.
- `check-corpus.sh` against b640e67d and again against c2f59d6f: only the new shape differs, and it is accepted with exit 0 and empty stderr.
- `cargo nextest run --release -p vyrn-cli`: 671 passed; no pin moved.
Time: about 50 minutes: fix and witnesses 10, `wasm2wat` and tally 10, gates 30.
Findings:
- `core_lambda`'s check `caps.len() == srcs.len()` no longer meets two captures with one source name from the core; it stays, because it also checks that the lifted body reads exactly the row's captures.
Left: nothing for #483; the PR closes it.
