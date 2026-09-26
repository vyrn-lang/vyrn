#### A join an arm owns holds no other arm's borrow (2026-09-26, `m7-small`)
RFC-0125, milestone M7. Fixes #518.
Decision: the lead's, option (a). The builder made an `if` or a `match` a borrow as soon as one arm yielded a borrow, so a fresh String from another arm was stored into a borrow and never released. `k.toString()` in #518 is one such arm. The rule now lives once, in `Builder::join_borrows`, which both joins call. A join is a borrow only when no arm yields a value the frame owns. Otherwise it owns its value, and the kernel's store check refuses a borrowed arm, the refusal the `if` over a `read` parameter already met. Option (b), a copy the builder inserts, was withdrawn.
Went: the two copies of "one arm is enough" in the `if` and `match` builders. The leak at four std sites, each an `if` join that borrowed a loop variable's field: std/vyx 2418 and 2422 (`a.value`), std/tw 283 and std/i18n 239 (`f.key`). The borrowed arm copies there.
Lines: `core.rs` 19 added, 15 removed (the helper and its doc; the two inline copies went). std 4 lines changed.
Refusals: 0 lost / 4 gained, one per witness below. Corpus: 0 gained. Manifest: untouched.
Licence:
- `vyrn check` over 539 corpus roots, after the std commit and after the rule: byte-identical, 460 accepted. Without the std copies the rule refuses 33 roots, all through the 4 std sites: 5 std, 11 site, 12 examples, 1 shape. That was the measurement that asked for them.
- the witnesses `r37` to `r40` in `refusals.rs`: a borrowed then an owned arm, and the reverse, for `if` and for `match`. The kernel states each (`VYRN_NO_KERNEL=1` accepts). The #518 probes, `match o { Some(s) => s, None => k.toString() }` and `if c { xs[0] } else { k.toString() }`, are r39 and r37.
- the shapes `a-borrowed-payload-copied-into-a-join-an-arm-owns` and `a-borrowed-element-copied-into-an-if-join-an-arm-owns`: both print 23 with the core walk on and off, with no free-audit line.
- `kernel` 27,061 / 0 / 0. `residue --ignored`: engine 173 clean, route 173 clean, 0 leaking. `vyrn doc --std --verify`: up to date.
- `coredrive` in two shards: 10,072 + 11,091 = 21,163 of 21,172 after the rule and at the tip.
- CLI nextest 680 passed. The frontend, lower and codegen unit suites: 1,124 passed.
Time: about 120 minutes: the cause 40, the measurement 30, the build and gates 50.
Findings:
- The 615 `match` joins a first instrument flagged were noise: a `panic` arm counts as one whose value the frame releases. No `match` join in the corpus is refused by the rule.
- The four std leaks ran in generators and site code, which `residue` does not run.
