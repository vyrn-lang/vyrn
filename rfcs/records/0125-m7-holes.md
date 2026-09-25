# A node in an interpolation hole names the line and column it sits on (2026-09-25, `m7-holes`)
RFC-0125, milestone M7.
Decision: the lead's, #471 after #502, which keys a lambda by line and column.
Went: the hole-relative positions. The lexer records where each `\{` hole starts (`lexer::Hole`), and `parse_hole` places the hole's tokens there; a hole's first line is offset by the hole's column, its later lines keep their own.
Stayed: a lex or parse error inside a hole stays anchored at the template, as `hole_parse_errors_are_anchored_at_the_template` pins.
Lines: `lexer.rs` grammar arm 577 to 592; `parser.rs` desugar 1,043 to 1,058, tests 1,948 to 1,961. The 30 lines carry a position the re-lex had lost; no statement of a rule left. Refusals: 0 lost / 0 gained, 1 moved. Manifest: untouched.
Licence, main `4f8a57e6` against the tip (first measured on `13dcc925`, the same counts):
- prediction before the build: corpus taken unchanged, because no corpus lambda sits in a hole; the witness's two lambdas and its `ap` instances leave the arm.
- `coredrive --ignored` with the manifest check: taken 21,094 to 21,094 of 21,172; carried end to end 1,666 to 1,666; 1 byte-identical, 167 run the same, 0 run apart. The manifest check is green.
- witness (the new shape, as `main`), `VYRN_FORM_TALLY` over `emit-wat`: base arm forms `ap` Return 2 + Var 4, `main` Var 6 + Binary 2; tip `ap` none, `main` Var 2. Output `6 10` on both.
- the lowering pin: `projection.vyrn` re-blessed, 66 lines, each `@1` to the line its hole sits on (61, 64, 67, 73); nothing else moved.
- `cargo nextest run --release -p vyrn-cli` 677 passed (diagnostics pin unchanged, formatter re-lex); parser census re-pinned; `vyrn-frontend` unit tests 698 passed, one new (`a_node_in_a_hole_names_the_line_and_column_it_sits_on`); `vyrn-lsp` 23 + 77 passed.
- `kernel` 27,061 accepted, 0 refused, 0 unlowered. `effects` 29,619 judged, 0 unattributed, 0 differ. `typed` 236,909 judged, 0 unjudged. `coretables` green.
- `check-corpus.sh`, `N:/wt-core` at main against the tip: `diff -r` shows the new shape (accepted, exit 0) and one moved line: `excl_alias.vyrn`'s exclusivity refusal from `1:0` to `10:0`, the line of its hole. Same sentence, same exit code. 446 accepted, 79 refused.
- #471's witness under `vyrn check`: the second line of the move refusal reads `line 5:`, where it read `line 1:`.
Time: about 85 minutes: work 20, gates and the base build 40, a rebase onto main after #502 merged and the gates again 25.
Findings:
- a refusal's line is observable behaviour: `excl_alias.vyrn` named line 1 for a call on line 10. Any tool that read a hole's diagnostic line read line 1.
- `residue` was not run: the change moves no emitted byte (manifest check green).
Left: nothing.
