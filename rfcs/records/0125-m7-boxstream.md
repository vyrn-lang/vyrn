#### `boxStream` is an `Effect` row that answers the box's address (2026-09-25, `m7-boxstream`)
RFC-0125, milestone M7.
Decision: the lead's (a `boxStream` row for the 4 `take` bodies). The shape, mine: `Spec::Effect` carries its result type, `Unit` for `writeStdout` and `close` and `Int` for `boxStream`, not a new kind, because the call is one operand at its name's type with a result. The prelude row's `Consume` already states the move, so the core and the kernel change nothing but the row.
Went: the arm's own `boxStream` emission; `stream_box` takes an operand closure, and the arm and the row share it. Stayed: `map` and `filter`, which also box, because they wait on `Op::Closure` (36).
Lines: `direct.rs` 22,239 to 22,252, `core.rs` 9,706 to 9,709, against the m7-stream tip on `4f8a57e6`. Refusals: 0 lost / 0 gained. Manifest: 4 rows (`membench`, `streamlazy`, `streamops`, `streamunfold`).
Licence, on `707694e6` and again at the tip on `4f8a57e6`:
- prediction sent before the build: taken +4, carried +4, `Rhs::Call` 63 to 59, 3 to 5 rows. Taken and rows hit; carried and `Rhs::Call` missed, because a builtin callee is no gap to that classifier: the core already carried `take`, and only the emitter lacked a reader.
- fast `coredrive` with the manifest written: taken 21,084 to 21,088 (+4) of 21,172 on `707694e6`; carried 21,042, 1,666 distinct, unchanged; 0 run apart. On `4f8a57e6`, with the manifest check: 21,125 to 21,129 (+4), carried 21,062, 1,676 distinct, `Rhs::Call` 43, 0 run apart.
- `wasm2wat` of the four, before and after (hashes equal to the manifest on both sides): one function moves in each, `take<Int64>`. The box's address goes to a fresh `i64` local before `newCursor`'s destination and operands are pushed, where the arm computed it in place: +1 local, +2 instructions per `take` call, frame 128 unchanged.
- shape `a-stream-boxed-answers-its-address`: both walks run the same, pin 0 break, 0 continue.
- `VYRN_LEAK_CHECK=1` native runs of the six stream examples: exit 0, empty stderr, stdout equal to main's binary's.
- `vyrn check` corpus diff against main's tree: empty but the new shapes, all accepted; 77 refused on both sides.
- `kernel` 27,061 / 0 / 0. `residue --ignored`: engine 173 clean, route 173 clean. CLI suite 677/677.
- pins by `VYRN_PIN=write`: `emitter-census`, `emitter-reads`, `surface` (`Type::Unit` 73 to 72: the `Effect` arm's result is the row's).
Time: about 105 minutes: 20 reading, 10 build, 45 gates and wat, 30 the rebases and their re-pins.
Findings:
- `carried` counts what the core states, not what an emitter reads, so a builtin with no reader moves only `taken`.
Left: `map` and `filter`, blocked by their lambdas' captures (`Op::Closure`).
