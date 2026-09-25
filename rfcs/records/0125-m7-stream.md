#### A stream's producers, its loop pull and `pullAt` are rows (2026-09-25, `m7-stream`)
RFC-0125, milestone M7.
Decision: the lead's. A `Stream<T>` name had no place because the stream builtins had no rows; give them rows. `close` keeps main's `Spec::Effect` row (#480): this track's `Spec::Closes` stated nothing `Fn_::effect` does not, and was dropped on the rebase.
- `fromArray`, `fromStep` and `unboxStream` are `Spec::Builds` rows. `stream_from_step`, `stream_unbox` and `stream_pull_at` take an operand closure, so the arm and the rows share one emission.
- A `for` over a stream parameter closes it: `Builder::stream_owed` and `Kernel::owned` read `must_use_param`. The core stated no close for a parameter, and the kernel refused the arm's release as a borrow's.
- The loop head is `@pull(modify s)`, a `Spec::Pulls` row that answers whether an element came; the element read is at the pull's name. `Opaque::Pull` is gone.
- `pullAt` is a `Spec::Builds` row whose element type is the row's `ret`, which `core_call` passes down.
- A pull reads the stream's own address local where its place is one (`core_addr_local`).
Went: `Opaque::Pull`. Stayed: the arm's stream code, because `take` (4 bodies) still waits on `boxStream`.
Lines: `direct.rs` 22,045 to 22,239, `core.rs` 9,665 to 9,706, `kernel.rs` 3,137 to 3,138, against `4f8a57e6`. The rows add readers; no arm left yet. Refusals: 0 lost / 0 gained. Manifest: 6 rows (`membench`, `stream`, `streamlazy`, `streammove`, `streamops`, `streamunfold`).
Licence, at the tip on `4f8a57e6`:
- fast `coredrive`, the manifest written in each commit: taken 21,094 on main to 21,125 (+31); carried 21,039 to 21,062, 1,666 to 1,676 distinct; `Val::Lit(Opaque)` 18 to 0; `Rhs::Call` 49 to 43; `Op::Closure` 35 to 36; `Stmt::ForIn` on the arm 38 to 3. 0 run apart. On `0f499e7b` the producer commit alone took 21,082 to 21,084, and `Stmt::While` on the core's rows fell from 2 to 0.
- instrument census (`instr-census.patch`), on `b640e67d`: the 11 stream `@lambda` bodies (`take` 4, `map` 4, `filter` 3) went from 4 clauses each (the `unboxStream` and `pullAt` rows and the two names they bind) to 0. Of the 5 take/stepped bodies, `stepped` went from 3 clauses to 0, and each `take` from 3 to 1: `boxStream`, which has no row. Refused bodies 253 to 222.
- `wasm2wat`, main against the tip: the loop body's `block` goes; the element lives in a local and the frame shrinks (80 to 64, 112 to 64 in `stream`); a producer or `pullAt` computes its operand before it pushes the destination and lands in the name's own slot (144 to 128 in each std/stream step); close takes a fresh local (main's `Effect`); in `membench` and `streamlazy`, a std/stream function is emitted at another index.
- `vyrn run --profile` ops, perf commit alone: `streamlazy` 385,626,922 to 385,584,636, `streamunfold` 61,456,888 to 61,414,796, `stream` 12,686 to 12,670; fewer in all six. Main's `streamlazy` is 385,286,764.
- `VYRN_LEAK_CHECK=1` native runs of the six, main's binary and the tip's: same stdout, exit 0, no audit line.
- `vyrn check` corpus diff against main's tree: empty but the new shapes, all accepted; 77 refused on both sides; the `streammove` refusals are unchanged.
- `kernel` 27,061 / 0 / 0. `residue --ignored`: engine 173 clean, route 173 clean, 0 leaking.
- pins by `VYRN_PIN=write` in each commit: `emitter-census`, `emitter-reads`, `surface`. CLI suite 677/677.
Time: about 450 minutes: 35 rebase and conflicts, 90 gates and censuses, 25 wat, 80 re-pinning after main moved three times, 30 the rebase onto `31a3e69a`, 190 the rebases onto `0f499e7b`, `ea737630` and `4f8a57e6` with a re-pin per commit, a coredrive bisect and the gates.
Findings:
- `lowered`: main's gate (#496) runs with the core walk off, so the arm answers every body and its peek class did not move: main's band (48..96) and compared floor (500,000) stand. The floor moves this branch once made are dropped.
- `coredrive`'s list of forms the rows carry lost `Stmt::While`: the producer commit took whole the bodies that held the rows' last two `while` statements.
- A `-x` rebase onto a main that moved the manifest keeps main's rows (`merge=pin`); regenerate the manifest in the same exec.
Left: `take`, blocked by a `boxStream` row.
