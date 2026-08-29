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

## First triage (same day): one row closed a quarter of the table

`intkeys`' 4-byte specimen led straight to a systemic finding: the
prelude's seeded row for `bytes` claimed a VIEW — "a header over the
String's OWN buffer" — and `lends` read the claim, so no `bytes` result
was ever released. But every engine implements a COPY (the interpreter
must: an `Rc<Vec<Val>>` cannot share a String's bytes, and
`__vyrn_str_bytes_range` is a malloc and a byte loop in both compiled
backends). Copy is the semantics; the row now says so (`prelude.rs`,
body emptied, return type answered), the ownership machinery frees the
result wherever it frees any fresh allocation, and the survey moved:
**clean 54, leaking 100 → 77, with `numparse`'s 2.5 MB outlier gone
entirely** (its bignum scratch was `bytes`-derived) and most remaining
rows shrunk (`knucleotide` 406 → 129 blocks, `codecbytes` 79 → 38). A
true zero-copy `bytes` is RFC-0109's stored-view question and would
arrive through that door — a locator, not a reclassification.

The triage also hardened the instrument itself: the teardown frees
through the audit table's arbitration now (`__vyrn_teardown_begin`),
because a global stored inside a `region` holds a block the arena
already freed at the closing brace — dynamic provenance the old
`place_owns` doc recorded as bluntness — and the strict double-free rule
must not apply to the teardown's own walk. `regionescape` moved from a
teardown double-free to an honest residue row.

## Second triage: the stream signature was a Slots slab, and the teardown was swallowing generic releases

The identical 5 × 192 signature across the three stream examples reduced
to one probe: ANY `Slots` global leaks exactly those five blocks — and
`std/stream`'s cursor slab is a `Slots` global (`cells`). The mechanism
was in the teardown itself: a GENERIC declared release
(`impl<T> Owned for Slots<T>`) solves its type arguments from `slot_ty`,
which reads the emission scope — empty in the teardown's fresh `Gen` — and
the Release arm's stated policy ("a drop this cannot emit is a leak,
never a wrong free") swallowed the miss silently. Parking the global's
binding in the scope for the duration of its drop lets the lookup answer,
and the streams, `tryplace`, and every `Slots`-holding program came back
clean. The instrument also gained a verbose form (`VYRN_LEAK_CHECK=2`)
that lists each residual block's size — the first thing a triage keys on.

Tallies after round two: **clean 58, leaking 92, zero double-frees** —
the leaking count ROSE from 77 because the `jsonread`/`json5` move fixes
restored nineteen json-heavy examples to the surveyable set; on the
constant denominator every number moved down.

## Third triage: an argument the coercion allocated, and a capture block that owned more than it freed

Two classes, one round. First, **the heapify argument**: `f([1, 2, 3])`
builds a fixed value the literal owns nothing of — and then the call
boundary's coercion into a growable `Array<T>` allocates a triple nobody
recorded, because the literal's own type deliberately answers nothing
(`Declared::type_of`'s array-literal rule). The plan records it now: the
argument-temporary machinery types such a literal FROM THE CALLEE'S
DECLARED PARAMETER, and the two call paths that coerce (the ordinary loop
and the higher-order twin) retarget the pushed free at the coerced
product, since the hook fired before the coercion on a value that owned
nothing. En route the finish check caught the NATIVE `fromJson` rewrite
embedding a clone of its payload argument — the direct backend's disease,
cured with the same scoped alias.

The class then earned two narrowings, both taught by the wasm generator
host (the one engine that RUNS `std/vyx`, whose CI job trapped where
every native gate stayed green). A `consume` position stands the record
down — the callee owns the coerced triple, and a caller-side free is a
second one (`vyxBuildModule(consume Array<VyxComp>)`). And the row is
recorded only for an element type that owns NO heap: the triple's buffer
is always freshly the caller's, but its elements are word copies of
whatever the literal held, and a binding the literal did not take
(`vyxUsesChildren([root])`, where a self-referring `VyxNode` stands down
from `owns_heap`, so `root` outlives the call into `VyxComp.root`) still
owns that heap — the deep free corrupted it and `vyxStrLit` trapped
three frames later. Heap-owning element types are a recorded leak of
this table, not a free. The direct backend's `expr_as` also retargets
the pending free at the coerced triple now, as the textual loops do —
the buffer free must land on the triple, not the frame-allocated fixed
value the tee saw.

Second, **the capture block**: capture is a take (`Gone::Captured` stops
the binding's own release), so a closure's block OWNS every heap value it
snapshot — and both the copy and the release were shallow. A copied fn
value shared its captured buffers; a released one freed the block and
left a captured String, or a nested fn value's own block, with no owner
(the `capturefn` 16-byte specimen was exactly that: an onward lambda's
inner value). All three deepened together, as they must — the old pin comment exposed the third piece (two lambdas over one binding shared ONE pointer, which is why the release had stayed shallow): `emit_capture` deep-copies heap values into the block, `__vyrn_fnval_copy`
duplicates heap captures per variant, the new `__vyrn_fnval_release`
walks them before the block goes, `deep_copy`/`deep_release` route `fn`
values through the pair, and the stream closer hands its step value to
the release twin instead of freeing the bare block. The direct backend
keeps its shallow pair for now — internally consistent, recorded here as
follow-up.

Tallies after round three: **clean 60, leaking 90, zero double-frees**,
with the fn-value family (`capturefn`, `streamops`/`streamlazy`/
`streamunfold` and the minimal probes) clean end to end.

## Fourth triage: the bindings the ownership pass could not type

One reading defect, three spellings — none of them a release-machinery
hole. `Declared::type_of` is the ownership pass's typing, and a binding
it answers `unknown` for gets no release row at all, `impl Owned`
notwithstanding. Three call shapes answered nothing:

- **A variant constructor.** `let doc = JObj([..])` — constructors are
  not functions, so `rets` had no row, and every unannotated `let` over
  an enum constructor leaked its whole tree (`jchain`'s 24 blocks; the
  declared `impl Owned for Json` never ran because no row asked for it).
  The variant table now maps each variant to the enum it constructs, and
  the `Call` arm answers it — guarded to `None` for a variant name two
  enums share, a generic enum (its bare name is an incomplete type), and
  the built-in sum constructors.
- **`toJson`.** The desugar was `json$emit(json$e<key>(arg))` — the whole
  encoded tree an ARGUMENT TEMPORARY, and a temporary whose release is a
  declared `impl Owned` call is exactly what the drains refuse (user
  code, observable timing). Every `toJson` leaked its tree: fourteen
  blocks for one small record. The desugar now calls a generated
  per-type wrapper that BINDS the tree (`let t: Json = enc(v)`), and
  block exit releases it the same way in all three engines. The render
  is bound before the return, because `return emit(t)` reads as `t`
  moving into the return and skips the release the wrapper exists for.
- **`fromJson`.** Type-directed, so `rets` had no row and the
  `Validation<T>` binding it lands in was unknown — the decoded value
  never released. A `type_of` arm answers `Validation<T>` from the
  call's own type argument.

Tallies after round four: **clean 63, leaking 87, zero double-frees** —
jchain clean; jsonbytes 600→385 blocks, enumcodec 279→175, domdemo
1586→970, graphql 3083→2758. The typing also surfaced one real defect
the unknown had been hiding: `vlog` stored `issues[0].message` — a
projection out of the `Invalid` binder — into a record that outlives
the arm; typed, the binder's release would have made that a dangle, and
movecheck now refuses it with the `.copy()` menu the example takes.
What remains in
the family is the fold's recorded loop-store conservatism (`out = out +
piece` in the emit/render loops — one grow-buffer block per append
spine, already listed under recorded conservatisms) and per-library
shapes (`htmltree`/`herofield` moved nothing and are next).

## The rule going forward

The instrument does NOT gate CI yet: gating requires this table to reach
zero rows or exemptions-with-reasons, and that triage is its own arc. What
gates today is the instrument itself (the two-sided pin) and the clean
examples staying clean wherever a row is closed. A row closed here should
name its mechanism the way the twenty-list did — this file is a list that
must be re-read, because a list that stops being re-read starts lying.
