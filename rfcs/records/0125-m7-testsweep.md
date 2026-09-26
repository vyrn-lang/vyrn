# The test sweep exempts the m7-hole rule, and CI runs it (2026-09-26, `m7-testsweep`)
RFC-0125, milestone M7.
Decision: the lead's. Add the rule to `LEFT_THE_CHECKER`, and run `testsweep` in CI's `judgments` job beside `kernel` and `typed`.
Went: nothing. Stayed: nothing.
Lines: `testsweep.rs` +8 (one entry and its reason). `ci.yml` +5 (one step and two comment lines). Refusals: 0 lost / 0 gained. Manifest: untouched.
Licence:
- before, on `aa63e0e1` and `05fa618d`: `testsweep --ignored` failed. It reported one program, `refusals.rs`'s `a_payload_of_a_type_that_declares_release_is_not_handed_on` (literal #1009, then #1025), accepted with `VYRN_NO_KERNEL=1` and refused by the kernel with "`kids` may not be handed to a `consume` parameter".
- after, on `2b08a1cf`: `testsweep --ignored` passed, 1 test in 348 s, release profile.
- `cargo fmt --all --check` passed. `ci.yml` parses, and the `judgments` job lists the step after `typed`.
Time: 25 minutes: 5 work, 20 gates.
Findings:
- The sweep failed from `e8728886` (2026-09-23) until this change. The rule is the kernel's alone, and CI did not run the sweep, so no gate reported it. The literal's index grew with `refusals.rs` (#937 on `513bae71`), which made the failure look like a new one each time.
- CI runs the step in the debug profile, as the other steps of the job do. Its time there is not measured.
Left: nothing.
