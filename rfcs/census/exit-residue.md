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

## Fifth triage: the fast path with no drain

One hole, both backends, convicted by `herofield`'s 660 identical
18-byte blocks — one per glyph cell. `acc = acc + f(..)` takes the
in-place append fast path when `acc` is an append candidate, and a
RETURNED accumulator is exactly that (a bare `return acc` does not ban;
a `print(acc)` does, which is why every `main`-shaped probe came back
clean and the class hid in functions). On that path the operand
partition was half-plumbed: an `@str`/`@concat`/`+` part is
`AlreadyFreed` and `free_str_temp` frees it, but a CALL-producer part
(`substring(..)`, `emitString(..)`, `emit(..)`) is `Released` — its row
pushed into `arg_frees` by the expression wrapper — and the fast path
was the one consumer with no drain. The rows sat below every later
mark, unreachable, until the function's clear. Both backends now drain
their mark after the appends, exactly as `gen_binary`/`call` do.

This was most of the render-loop residue: the emit loops of `std/json`
(`out + emitString(f.key) + ":" + emit(f.value)`) and the render loops
of `std/html` concatenate call results into a returned accumulator, per
field, per node. Movement: herofield 667→7 blocks, htmltree 799→589,
domdemo 970→472, graphql 2758→2275, jsonbytes 385→251, enumcodec
175→133 — and the method note stands corrected: a probe that prints two
lines hides the audit line from `tail -2` (stdout flushes after
stderr), so every probe verdict is now read off the EXIT CODE.

Tallies after round five: **clean 63, leaking 87, zero double-frees** —
the example counts hold (each shrunken row still holds a remainder),
but the block totals across the family dropped by roughly a third.
What herofield still holds (7 blocks) is its outermost results and the
float-bits appends; what the render family still holds is the decode
scratch and per-library shapes, next in line.

## Sixth triage: one conviction, two refusals, and a bisect that lied

The conviction: **`keyed` read as a lender.** `std/html`'s `keyed`
matched its `consume Html` param with a plain `match`, so the binders
read as projections, the return as a borrow, and `keyed` as a lender —
which the closure spread through `itemRow`-shaped callers and every
tree they built was never released (htmltree 589→187 blocks). One
word: `match consume node`. `VYRN_LEND_DUMP=1` prints the lending
seeds — the round's instrument; the html corpus's seed list is now
exactly `attrKey` (a real lender, by design) and `num$scan`.

The refusals, each tried and backed out after the wasm generator host
corrupted `std/vyx`'s output (a stray `\u{1}` in generated code):

- **Answering a `fn`-typed binding's return type** (`df(13)` where
  `let df = d.run`) records the result as a caller-owned temporary,
  and a lambda may return a CAPTURE or a parameter, which no reading
  of the call site can see. `fnvalarg`'s seven blocks are the recorded
  price of fn-value opacity; an arm needs a pin proving the
  capture-returning shape copies on return first.
- **Dropping a wrapped lend when the borrow's type owns no heap**
  (`JBool(b) => JBool(b)` in a declared `copy` taints the whole copy
  as a lender). The filter read the type off movecheck's WIDENED
  `type_of`, and somewhere in `std/vyx` that reading lies — a real
  lend was dropped and the generator freed a borrowed buffer. This was
  the actual corrupter; the declared-copy-as-lender defect it aimed at
  is real and still open, and wants a narrower guard than a widened
  type reading.

Two operational lessons, both earned twice over: a revert-confirmation
must run the SAME ENGINE that failed — a featureless rebuild ran the
interpreter and "confirmed" an innocent change guilty — and a
corrupting generator build poisons `~/.vyrn/cache/gen` past its
revert (the cache key does not see a same-version compiler rebuild),
so purge the gen cache after backing one out.

Also recorded as-is: `argsdemo`'s single 0-byte block (an empty argv
buffer pinned by an escape conservatism, zero bytes, not worth its
fix); `fnvalstore`'s six blocks (the `Gone::Captured` class). `domdemo`
(472 blocks) reclaims every binding and still leaks — its residue is
machinery, not bindings, and it anchors round seven.

Tallies after round six: **clean 63, leaking 87, zero double-frees** —
counts hold; htmltree 589→187 under the one lending correction.

## Seventh triage: a box the walk never saw, and a return that gave up too much

Two classes, and between them most of `domdemo`'s anchor. Convicted by
descent: `domdemo` 472 → one `toJson(view())` at 395 → one small tree
at 13 → one node kind at a time — `emit(JBool(true))` leaked exactly
one 1-byte block, and `let t: Json = JBool(true)` alone still did.

- **The payload box.** `release_enum`'s walk was gated on
  `owns_heap` per variant and per slot — but BOXED-ness is
  `unbox_payload`'s criterion (any payload that is not an `i64` word
  travels in a box), and the two are different questions: a `Bool`
  payload owns nothing and is boxed anyway. Every `JBool` leaked its
  1-byte box on every release, `String` payloads passing only because
  they own heap. The walk now asks both questions.
- **The return that marked everything.** Rule 3's conservative marking
  at a `return` (`gave_up`) took every binding the returned expression
  reads — so `return fromBytesOr(out, ..)`, `return match
  stringFromBytes(out) {..}`, and `return acc + "}"` each left their
  accumulator unreleased, one scratch buffer per call, twelve spellings
  in `std/` and every caller's own. The marking is narrowed at returns
  (`gave_up_returned`): a KNOWN function's owned result contains none
  of its read arguments (rule 3 refuses returning borrows; lender and
  retainer positions are settled by the `facts()` post-passes reading
  `row.passed`), the value-position joins recurse per arm, an
  addition's operands are always copied, and a condition is a test.
  Constructors, views, `blackBox`-shaped param-identity rows, and
  `fn`-typed locals keep the conservative walk.

Movement: `herofield` CLEAN, `domdemo` 472→84 blocks, `jsonbytes`
251→35, `enumcodec` 133→66, `graphql` 2275→1309; the `toJson`/`emit`
micro-probes are at zero. `htmltree` holds 187 (its residue is
elsewhere) and the decode side keeps ~6 per `fromJson` — both queued.

The narrowing then UNPINNED two latent `std/graphql` defects the old
conservatism had been keeping alive as leaks — parity's wasm run
convicted them (`{"data":{:` where the key had been, then OOM; the
native allocator's reuse pattern hid both):

- `gqlAnswerOne` built its error path as `[JStr(sel.key)]` — a literal
  holding a BORROW of the selection key through the constructor
  position. Releasing `at` freed the key under `q.sels`. `.copy()`.
- `gqlProjectCore`'s leaf arms re-wrapped borrowed payload strings
  (`JStr(s)`, `JNum(n)`) into the owned answer — the nested path takes
  that value into the response while the resolved document is
  released. `.copy()`, exactly as the leaf position above them always
  did.

The class behind both: the constructor position ACCEPTS a projection
(the value it builds "holds the argument"), so a literal can smuggle a
borrow into a value the machinery then releases as owned. Recorded as
an open language question — the store rule refuses this shape
everywhere else. The bisect instrument that found them:
`VYRN_RET_NARROW_OFF=all|call|join|add` disables the narrowing by arm,
`VYRN_RET_NARROW_SKIP=fn1,fn2` per function — eight builds from 102
candidates to one. (And the round-six cache rule earned a third
telling: the bisect's broken intermediate builds re-poisoned
`~/.vyrn/cache/gen`, and the site export failed from the cache after
the code was already fixed.)

Tallies after round seven: **clean 69, leaking 81, zero double-frees**
— six examples flipped clean in one round, the largest single-round
movement since the first triage.

## Eighth triage: the wrapper that moved its accumulator, and the boxes a consume-match left behind

Two classes, both convicted from `htmltree`'s stubborn 187 by per-kind
probes of `toHtmlString`:

- **The consume-wrapper spine.** `std/html`'s escape loops wrote `out =
  appendBytes(out, ..)` where `appendBytes` took `dst: consume` — a
  consume re-bound inside a loop is a take the revive machinery
  refuses, so every escape buffer was marked moved and never released,
  even on strings that never took the escaping arm (the take is
  static). The wrapper predates RFC-0115's `append`, whose receiver is
  `modify` — no move at all. Six sites now call `out.append(..)`
  directly and the wrapper is gone.
- **The consume-match's boxes.** `match consume node` TAKES the
  scrutinee, so the fn-exit release that used to walk the payload
  boxes never runs — and the ordinary match lowering extracted the
  payloads and left the boxes behind (`keyed` after round six leaked
  all three of `El`'s per node). The match frees each extracted slot's
  box now, gated three ways: the scrutinee must be a `consume`, no
  drop row may exist for the match (the fall-through release walks
  boxes where one does), and the body must not be a declared `release`
  — whose CALLER walks the boxes after the call, which is where the
  first attempt double-freed the whole corpus before the gate landed.

Movement: htmltree 187→25 blocks, domdemo 84→6, vyxdemo 22→13.
`cat1`-style single blocks map to the recorded round-3 heapify class
(heap-owning element literals); `graphql`'s ~1300 is untouched by
everything so far and anchors round nine.

Tallies after round eight: **clean 71, leaking 79, zero double-frees**.

## Ninth triage: the closure's result becomes its caller's, and the fn-call arm comes home

`graphql`'s ~1300 reduced to one shape in three probes: parse alone is
CLEAN, and every leaked class flows from calls through `fn`-typed
values — `let tref = schema(owner, s.name)` at every node of every
walk, untyped since round six's refusal, so neither the binding nor
the argument-temporary machinery ever released a result.

Round six refused the typing for want of a pin, and round nine built
the pin: the emitted lambda body for `n -> cap` is `ret ptr %t2` — the
capture returned RAW, no copy, two calls handing back one pointer,
saved from a double-free today only by the untyped leak itself. So
rule 3 now reaches closures: **a lambda may not return a captured heap
value raw** — movecheck refuses it with the same `.copy()` menu a
function returning a borrow gets (an `Expr`-bodied lambda's result;
block-bodied returns are recorded follow-up). The corpus needed ZERO
fixes — no example or std lambda returns a capture — and with every
`fn` value's result thereby owned, `Declared::type_of` answers the
binding's own return type and the drains free what the calls build.

Movement: graphql 1321→882 blocks, fnvalarg 7→2, gq micro-probe 24→20.

Tallies after round nine: **clean 71, leaking 79, zero double-frees**
— counts hold, with the fn-value family's block totals down by a
third across the corpus.

## Tenth triage: the constructor door closes, and the constructed temporary is finally owned

One `gqlErrorBody` call leaked seven blocks, and the descent named the
oldest exclusion in the argument-temporary machinery: a
CONSTRUCTOR-built argument (`emit(JObj(..))`) was never recorded — "a
constructor's payload is the constructed value's" — so no
`emit(tree)`-shaped call ever freed its tree, across `std/json`,
`std/graphql`, `std/symbolmap` and every caller.

Admitting those temporaries requires that a constructed value OWN its
payloads — which is exactly round seven's open language question, now
settled as a rule: **a constructor position no longer accepts a bare
projection or borrowed name** (a `consume` take transfers ownership
and stays legal; a whole owned name still moves in). The store rule's
`.copy()` menu applies, and the old pin that asserted
recorded-not-refused flipped to assert the refusal. The corpus sweep
found ~25 real borrow-smuggles across ten std modules — `Some(next)`
in `std/args`, `Some(best)` in `std/contract`, `attrKey`'s
`Some(k)` (the one DESIGNED lender, now an owner), `JStr(message)` in
`gqlErrorBody`, `VNElem(tag.name, ..)` through `std/vyx`'s parser,
`vyxShiftNode`'s whole rebuild-from-binders — plus two examples and
one generator template (`Err(consume arg.err)` in `graphqlServer`'s
emitted dispatcher). Every fix is the copy or take the value always
owed.

With the door closed, `note_arg_temp` records constructor producers
like any other; the variant table types them (round four's machinery),
and the drains free them. Movement: `gqlErrorBody` 7→0, the gqlAnswer
micro-probe 20→3, `emit`/`toJson` probes stay 0, graphql 882→308,
vyxdemo 22→13.

The sweep then had to mean "every": the subdirectory apps
(`bin`/`shelf`/`fullstack`), the site tree, and the GENERATOR TEMPLATES
whose emitted code is also a corpus. The refusal surfaced and closed:
`std/rpc`'s `Rejected(bag.issues)` (→ `consume`), `std/openapi`'s
`$ref`-walk rebuilding from binders and its `$id` field, `std/vyx`'s
dynamic `:attr` hoist (`A(name, (expr).copy())` in emitted text — a
user's `:value={prop}` must not smuggle the prop) and its lazy-page
wrapper (`page(d: consume T)` so `Ready(consume d)` is a transfer),
`std/icons`' two plain matches over `consume` params (→ `match
consume`, the `keyed` lesson again) and its `glyph` template, the two
apps' middleware fold (`hit = match hit { Some(r) => Some(r), .. }` in
a loop — restructured to an `if let` over the fresh call), and a dozen
site view helpers. A mechanical fixer (scratchpad `autocopy2.py`)
applied the menu's `.copy()` wherever the diagnostic named a line;
`match consume` and `consume` were chosen by hand where a transfer was
the truth. Late finds: `std/bench`'s report writer, the vyx `v-html`
hoist (`Raw((expr).copy())` in emitted text), and three test pins that
asserted the old spellings — including parity's retained-argument
fixture, whose doc already said "until the constructor hole was
closed" and whose day this was. One tooling lesson: the mechanical
fixer must never touch QUOTED text — it rewrote a JSON-LD key string
`"url"` to `"url.copy()"` alongside the value it was fixing, and the
site metadata test caught the corrupted record.

Tallies after round ten: **clean 72, leaking 78, zero double-frees**.

## Eleventh triage: the error return that poisoned every path, and the box `?` left behind

The decode side, convicted by shrinking `parseJson` probes: every call
leaked exactly one input-sized block and one 16-byte block per parsed
value, on every input, including `"7"`.

- **The input-sized block** was the cursor record's byte buffer, and
  the mechanism was round seven's marking one arm short: `return
  Err(errAt(p, "trailing content ..."))` at the BOTTOM of `parseJson`
  reads `p` through a constructor, the constructor arm fell to the
  conservative walk, and `p` was marked Returned — on EVERY path of
  the function, early returns included, because the marking is a
  row property and not a path one. `gave_up_returned` now recurses
  into a returned constructor's arguments (a call argument's reads
  are rule 3's business, exactly as at the top level; a direct place
  argument keeps the conservative walk, and the call walk has already
  moved or refused it at the constructor position).
- **The 16-byte block** was the `?` operator's: a success payload
  wider than a word (a `Json` is two) travels BOXED inside its
  `Result`, and `gen_try` loaded the value out and left the box — no
  row anywhere names a `?` operand's temporary. The Ok branch frees
  the box now, gated to NON-PLACE operands (`r?` over a binding leaves
  the box to the binding's own release) — one block per
  `parseValue(p, ..)?`, one per parsed JSON value in the corpus.

Movement: every `parseJson` micro-probe CLEAN, `fromJson` 6→2 blocks
per call, graphql 308→94, enumcodec 66→25.

Tallies after round eleven: **clean 72, leaking 78, zero double-frees**
— counts hold; the decode family's block totals drop by two thirds.

## Twelfth triage: the comparison nobody counted

`gq1`'s last two blocks were two copies of `"Book"` — the results of
`schema(ty, "") == ""` inside `gqlCheckSel`, one per executed GraphQL
selection. A String COMPARISON's operands were in nobody's operand
class: the lowering copies nothing and frees nothing, and no row named
the temporary — the same finding-3 shape the census caught for `+`,
one operator over. Three symmetric pieces close it: movecheck notes
comparison operands under the `@concat` spelling (so `arg_verdict`'s
partition holds — an operand that allocated its own value is the
operator's, a call result is the drain's), the textual backend's
strcmp arm runs `free_str_temp` on both halves, and the direct
backend's tee does the same. `=~`'s left operand is the recorded
remainder of the operator class.

Movement: the gqlAnswer micro-pipeline is at ZERO (`gq1`, `gq3`, and
every `parseJson`/`toJson`/`emit`/render probe), graphql 94→53.

Tallies after round twelve: **clean 72, leaking 78, zero double-frees**
— counts hold; graphql sits at 53 blocks, down from 3,905 at the first
survey.

## Thirteenth triage: the temporary the match never released, and the lender question laid to rest

Three pieces, all on the decode tail:

- **The temp-scrutinee box.** `readDoc`'s `return match parseJson(src)
  { Ok(j) => out.push(j), .. }` destructures a TEMPORARY whose boxed
  payload (a `Json` is two words) nobody owns once an arm takes it —
  the scrutinee's drop row is cancelled by the take, and round eight's
  per-arm box free was gated to `consume` scrutinees only. The gate
  widens to any NON-PLACE scrutinee, and the Option/Result match path
  (`gen_arm_body`) frees the box exactly as the enum path has since
  round eight. One 16-byte block per `fromJson`, gone; `fjW`-shaped
  probes fully clean.
- **The copy-lender filter, landed at last.** Round six refused it
  while two real dangles sat behind it; rounds seven and ten fixed
  those and closed the constructor door, and the filter now lands
  clean: a wrapped borrow whose KNOWN type owns no heap is no lend, so
  a declared `copy`'s scalar arms (`JBool(b) => JBool(b)`) no longer
  taint it — and through it `fieldAt`/`elemAt` and every
  `copyJson`-forwarding reader — into lenders. The decode link's
  lending seed list is now EMPTY.
- **Template hygiene.** The jsondec top walks its doc by `consume` and
  its issue returns take the accumulator.

- **The consume-for's abandoned buffer — tried and REFUSED.** The
  32/64-byte scaling block is the decode top's snapshot: `for x in
  consume val { return Valid(x) }` hands the element out, the handover
  marks the snapshot row gone, and the documented conservatism drops
  the whole row, buffer included. Downgrading the marked row to a
  buffer-only free bought clean=78 and FOURTEEN parity divergences —
  `aliascontext` trapping outright on wasm — because a `Fate::Moved`
  on that row does not always mean "an element left", and freeing the
  buffer where it means something else is a use-after-free. Reverted;
  the snapshot buffer stays the recorded price until the row can say
  WHICH take marked it. (The survey's honest gain for the round is
  what the other two fixes bought.)

Recorded for later: that row refinement, and the `Err(e)` arm's
message when the arm only reads it (`fjE`'s 53 bytes).

Movement: `fromJson` at 1 block per call, graphql 53→14 blocks (3,905
at the first survey), enumcodec 25→12, `fjW` clean.

Tallies after round thirteen: **clean 74, leaking 76, zero
double-frees** — the survey crosses its halfway mark, parity holding
at zero divergences after the refusal.

## Fourteenth triage: the record nobody recorded

`jsonbytes`' thirty-two identical 20-byte blocks — frozen since round
seven — were one field buffer per `ctlJson` call: `toJson(Esc { s:
ctl(b) })` builds a STRUCT-LITERAL argument, and struct literals were
the one producer the argument-temporary record still fell through
(`_ => return`). Admitted on round ten's terms: the literal's fields
already take the store discipline (moves, takes, or refused borrows),
so the constructed record owns everything in it and the drain's deep
free is sound. The row types from the literal's own name, producer
`@record`, `consume`/sink positions standing down as everywhere.

Movement: jsonbytes 35→1 blocks.

Tallies after round fourteen: **clean 74, leaking 76, zero
double-frees** — counts hold, parity at zero divergences.

## Fifteenth triage: the copy that abandoned its original

`onPair`'s 36 bytes, twelve times per htmltree run: the append fast
path's take-ownership step (`app.own`) COPIES the accumulator's
current value into a fresh shadow block — correct when the value is a
borrow (`let mut out = someParam` must not free the param's buffer) —
and ABANDONED the original when it was owned, which is what the
opening line of every renderer (`let mut out = " data-on-" + event +
..`) is. The plan already answers the question per statement:
`store_owned_at` proved the place owns its value, so the take frees
what it copied out of (`emit_str_append_owned`, `free_taken`); a
borrow answers false and keeps the old behavior, and a static literal
init is covered by `str_free`'s own cap guard. Loop stores still
answer false — the loop conservatism keeps its recorded block
(`cat1`'s 24 bytes).

Movement: `domdemo` CLEAN, htmltree 25→9 blocks, vyxdemo 13→7, every
`onPair`/`toHtmlString` micro-probe at zero.

Tallies after round fifteen: **clean 75, leaking 75, zero
double-frees** — dead even, from 54/~100 at the first survey. The
append-count pin moved to equality: one store-side free each,
differently placed.

## The rule going forward

The instrument does NOT gate CI yet: gating requires this table to reach
zero rows or exemptions-with-reasons, and that triage is its own arc. What
gates today is the instrument itself (the two-sided pin) and the clean
examples staying clean wherever a row is closed. A row closed here should
name its mechanism the way the twenty-list did — this file is a list that
must be re-read, because a list that stops being re-read starts lying.
