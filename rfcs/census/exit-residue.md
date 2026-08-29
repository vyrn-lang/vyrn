# Exit residue: what the leak-check instrument found on its first survey

- **Date:** 2026-08-29, at the RFC-0124 head.
- **Instrument:** `VYRN_LEAK_CHECK=1` (RFC-0114 §25's completeness half):
  the audit table arms, `@__vyrn_globals_teardown` drops module state in
  reverse declaration order after `main`, and `__vyrn_audit_exit` fails the
  process (exit 135) if any block remains — births equal frees, as a
  checked exit condition. Two-sided pinned in `parity.rs`
  (`leak_check_is_two_sided`).
- **Method:** every checkable example built natively and run once under the
  instrument, stdin fixtures honored. Exit 135 rows below; everything else
  came back clean, including the module-state-heavy witnesses (`mapkey`,
  `protoplace`, `statemod`-class programs whose globals the teardown walks).

## The finding, in one sentence

Roughly a hundred of 170 examples hold residue at exit that the peak rows
cannot see — mostly tens of blocks and a few hundred bytes, with a handful
of large outliers — and the classes are visibly mixed: recorded
conservatisms, runtime machinery, and candidate real leaks.

## The outliers (worth their own triage first)

| example | residue | first hypothesis |
| --- | --- | --- |
| numparse | 784 blocks, 2,549,990 bytes | the float-parse exact path's bignum scratch |
| freelist | 100,000 blocks, 2.4 MB | the example IS a free-list; likely holds by design |
| regexredux | 4,071 blocks, 423 KB | per-match strings in the reduction loop |
| graphql | 3,905 blocks, 115 KB | generator/parse structures |
| jsondepth | 799 blocks, 75 KB | deep-tree recursion path |
| domdemo | 1,916 blocks, 77 KB | vyx/dom tree machinery |
| rest | 1,876 blocks, 46 KB | handler/fn-value captures |
| revcomp | 509 blocks, 33 KB | line-buffer path |
| threeengines | 599 blocks, 32 KB | mixed |
| knucleotide | 406 blocks, 23 KB | counting structures |

## The visible classes

- **Recorded conservatisms.** The fold refuses stores it cannot prove
  (loop-shared stores, lent values) — `s = p.name.copy()` in a loop is the
  pinned example, and every instance of that class now has a number
  attached instead of a shrug.
- **Machinery.** Stored-fn capture blocks (`capturefn`: exactly one block of
  16 bytes), stream cursor cells (`streamops`/`streamlazy`/`streamunfold`:
  an identical 5 × 192 signature), `args()`'s empty-array `malloc(0)`
  (`argsdemo`: one block, zero bytes). Each is a candidate for either a real
  release or an explicit exemption with a reason.
- **Candidate real leaks.** Small constants per program (4–100 bytes) that
  no peak row could ever see — `intkeys`' single 4-byte block is the
  cleanest specimen: one tiny string, one missing release, invisible to
  every existing gate.

## The rule going forward

The instrument does NOT gate CI yet: gating requires this table to reach
zero rows or exemptions-with-reasons, and that triage is its own arc. What
gates today is the instrument itself (the two-sided pin) and the clean
examples staying clean wherever a row is closed. A row closed here should
name its mechanism the way the twenty-list did — this file is a list that
must be re-read, because a list that stops being re-read starts lying.
