# RFC-0093 — A Take Is a Move Out of a Place

- **Status:** **M1 and M2 landed.** The design predicted 44 copies; the migration
  removed **45**, and gave four distorted public signatures their return values
  back. M2 carries the hole set through the release walk, so a drained record is
  reclaimed minus the places it gave away. "M1 as landed" and "M2 as landed" at
  the bottom say what the design got right and what it got wrong.
- **Depends on:** RFC-0089 rules 1, 2 and 4 (landed), RFC-0092 M1 (landed),
  RFC-0011 (`swapRemove`, landed). RFC-0092 M3 is **not** a dependency and this
  RFC states what M3 must do about it.
- **Principle:** RFC-0089 rule 4 says a place owns its contents. It does not say
  a place must keep them. Give the program a way to say the transfer.

---

## The question

RFC-0092 M1 migrated 92 sites and landed **116 `.copy()`, 7 shape changes and
zero `consume`**. M1 wrote down what the zero meant:

> There is no way to move a field or a payload out of a place the frame owns.
> `consume` is a parameter capability and `for x in consume xs` is a loop form;
> neither reaches `let d = f() ; use(d.title)`.

That is this RFC's question, and it has a precedent one milestone chain back.
Phase 4b measured 207 loop-variable stores that could only become copies, said
the copies were the wrong answer, and added `for x in consume xs` (PR #74). The
shape repeats: a migration that can only produce copies, and a count that says
the copies are the wrong answer.

The compiler already says the words. Checked at `f7a37e8`:

```text
hole.vyrn:10:0: `d.title` may not be stored into `push(..)` — it is read out of a place that owns it
  fix: `d.title.copy()` if both sides need a value
```

```text
hole2.vyrn:10:0: `b.tags` may not be consumed — a place owns its contents, so taking
`b.tags` out of `b` would leave a hole
  fix: `for .. in consume b` if the loop should take the whole value
  fix: `b.tags.copy()` if `b` is still needed
```

The second one is `check_consuming_iter`
(`compiler/vyrn-frontend/src/movecheck.rs:1193-1201`). It is the whole design in
one refusal: the language knows what the reader wants, names the hole as the
reason it will not do it, and offers taking the **whole root** instead.

**And the first fix on that menu does not typecheck when the root is a record.**
`for t in consume b` over a `Bag = { tags: Array<String> }` answers

```text
`for` needs an Array, a String, or a type that declares `impl Iterate`
(a `size` method and a `place nth`), found { tags: Array<String> }
```

so the reader is sent to a second error. That is the same defect RFC-0092 exists
to remove — one question, two answers — and it is a one-line consequence of
there being no take.

---

## The count

The corpus is migrated, so this is a measurement and not an estimate. Every
`.copy()` PR #109 added is in the tree, and every one was read in its enclosing
function.

Measured at `f7a37e8` over 230 `.vyrn`/`.vyx` files: **499 `.copy()` occurrences,
up from 383 at `6a8db90`** — the net 116 PR #109's body reports. 123 occurrences
were added and 7 removed; 114 of the added ones are code rather than a comment.
475 of the 499 are code, and 470 of those are hand-written rather than inside an
emitted string.

Each of the 114 was classified by whether a take could replace it:

| bucket | what it means | count |
|---|---|---|
| **A** | the root is frame-owned and is **never read again** | **22** |
| **B-disjoint** | frame-owned root, read again only through **other** fields | **22** |
| B-whole | frame-owned root, the same path or the whole value read again | 16 |
| C | the root is a borrowed parameter, `self`, or module state | 50 |
| D | a whole value or a loop variable, not a projection | 4 |

**A + B-disjoint is 44, and the design decides which of the two numbers applies.**
A whole-root take buys 22. The path-keyed hole — answer (c) above — buys 44.

That is the same conclusion the worked examples reach from the other side, and it
is worth seeing why the two halves are the same shape. B-disjoint is dominated by
a single record literal that reads one field by take and the others by name:

```vyrn
return Scanned { ok: true, neg: neg, d: t.d.copy(), dp: t.dp, trunc: t.trunc }
```

(`std/num.vyrn:233`). And by the nine-line block at `std/vyx.vyrn:1431-1439`,
where nine identical field copies drain one record and **only the ninth is A**,
because it is the last. A rule where the ninth line is legal and the first eight
are not is not a rule anybody can teach.

**Three more sites are within reach and are not counted.** `std/vyx.vyrn:1827`,
`:2185` and `:2222` re-read the root only on the **other** branch of an `if`.
`movecheck` clones its consumption map per branch already
(`movecheck.rs:1865-1867`), so the read check is per-branch and these convert; the
union rule below is about the drop set, not the read set. They are left out of
44 because counting a case the migration has not run is how a number stops being
a measurement.

### Per kind, and this is what decides "one mechanism or three"

| kind of place | A sites |
|---|---|
| a record field | **20** |
| a container element | 2 |
| an enum payload | **0** |

**The enum payload is zero, and it is zero for a good reason.** A `match` over a
fresh call result already binds owners — `pattern_binders` asks
`place_path(scrutinee)` (`movecheck.rs:1153`) and a call is not a place. So the
payload copies that remain are all matches over a **borrowed** parameter or
`self`, which no take rule reaches. This RFC's M1 buys zero copies by itself,
and the milestone section says so rather than hiding it.

**The container element is two**, and both are `names.push(parts[0].copy())` in
`std/i18n` over a `split` result that dies after. `parts.swapRemove(0)` writes
them **today**. That is the element case answering for itself.

### Two things the 44 does not include

**183 of the 470 hand-written copies are C**, off a borrowed parameter or `self`,
and no rule in this RFC moves any of them. The two biggest clusters in the whole
corpus are `std/http`'s `httpCopy` (nine field copies off `r: Route`) and
`std/ui`'s five `Head` builders (twenty copies off `h: Head`). They need
`consume` on the **signature**, which is a decision about the caller and not
about this RFC.

**Fourteen copies live inside generated code and are not in the 470.** Nine sit
in emitted string literals (`std/rpc.vyrn:308` alone emits three into every
client and every server module a project generates) and five in `vyrn"""` code
quotes in `std/ui`. They expand per generated module, so their real multiplicity
is a property of the project being built. **They are deliberately not counted**,
because a number that depends on which project you compile is not a corpus
measurement — but they point the same way.

---

## The spelling

**`consume <place>`, as a prefix on a place expression.**

`for x in consume xs` reused an existing word at a new position rather than
inventing one. This is the same move a third time, and the same word.

Three reasons, in order of weight.

1. **The word already means this.** `consume` is the capability that says
   ownership transfers here. A parameter declares it; a loop declares it; an
   expression would declare it. RFC-0089's vocabulary discipline is one word one
   meaning, and `take` would be a second word for the meaning `consume` already
   carries.
2. **The reader has already been told the word.** `check_consuming_iter`'s menu
   offers `consume` as the fix for exactly this problem — for the whole root,
   because that is all it can offer. Making `consume b.tags` legal changes what
   the menu can say, not what vocabulary the reader has to learn.
3. **The parse is the trick that already works twice.** `consume` is contextual:
   a capability when an identifier follows it, a call to a user function named
   `consume` when `(` follows it (`parser.rs:2544`, `parser.rs:3638-3642`). A
   third position is the same test.

It reads correctly because the sentence is the same in all three positions: *the
thing after `consume` is given up here.*

```vyrn
fn consumingRead(d: consume Doc) -> String {   // the parameter is given up by the caller
    return d.title.copy()
}
```

```vyrn
for t in consume tags {                        // the container is given up by the frame
    out.push(t)
}
```

```vyrn
out.push(consume d.title)                      // the field is given up by the record
```

**It is a prefix and not a method.** `d.title.take()` would follow
`swapRemove`'s precedent — a builtin that needs the binding rather than the
value (`direct.rs:5937`) — and it is rejected below. The short reason is that a
method reads as an operation on a value, and this is an operation on a place.

---

## The rule

> **`consume p` yields the value at place `p` as an owned value. The place `p`
> is dead from that point. Its root is not — only the path taken.**

That second sentence is the whole design decision, and "What happens to the place
afterwards" below is where it is argued and priced.

Three refusals, and all three are already written. `check_consuming_iter` is the
function; a take reuses it with one branch deleted.

| refusal | today's site | why it stays |
|---|---|---|
| `consume` on something that is not a place | `movecheck.rs:1183` | there is nothing to take from; the value is already owned |
| `consume` of a borrowed root | `movecheck.rs:1212` | the frame does not own it, so it may not give it away |
| `consume` of module state | `movecheck.rs:1204` | nothing may take module state, and that is true of every caller |
| ~~`consume` of a projection (`path != root`)~~ | `movecheck.rs:1193` | **this is the branch that goes** |

The fourth refusal is the gap. Deleting it makes `for t in consume b.tags`
legal, which is a relaxation of an existing error rather than a new form, and it
is what makes the expression prefix and the loop form agree.

### What happens to the place afterwards

Three answers were possible. The design takes the third and refuses the other
two for measured reasons.

**(a) The take is a move of the whole root.** `consume d.title` kills `d`.
Cheapest to implement — `took(&root, ..)` and `consumed.insert(root, ..)` are two
lines that already exist and are already called by the consuming loop
(`movecheck.rs:2027-2042`). **Refused, because it does not fix the corpus.** The
sites that want a take read another field afterwards. `std/vyx`'s caller reads
`er.next` after it wants `er.node`; `std/graphql`'s caller reads `parsed.err`
after it wants `parsed.args`. Under (a) both are a use-after-move and the copy
comes straight back.

**(b) A hole with a runtime flag.** The general partial move: any field, any
control flow, a drop that consults a per-field flag at run time. **Refused.**
Vyrn has no drop flags and RFC-0089 rule 5 says `Ref<T>` is the one runtime
mechanism and the only one. A drop flag would be a second.

**(c) A hole the compiler knows statically, and a leak where it cannot.**
Taken. The rule:

- **The taken PATH is dead, not the root.** `consume er.node` refuses a later
  `er.node` and allows a later `er.next`. The consumption record is keyed by
  path rather than by root; `Consumed` is already a `String`-keyed map and
  `root_of` (`movecheck.rs:646`) already exists to compare the two.
- **A reassignment revives the path.** `er.node = v` makes `er.node` readable
  again. `movecheck` already does this for a whole variable — its own module
  comment says *"reassignment revives a variable"* (`movecheck.rs:12`).
- **At a branch join the hole set is the UNION.** A field taken on one path only
  is treated as taken on both. On the path that did not take it, the value is
  **leaked** rather than freed twice.

The union is the whole reason (c) needs no runtime flag, and it is the direction
this compiler already fails in. `vyrn why --memory` over the corpus reports
2,127 bindings not reclaimed today (RFC-0092 M1). A leak is a row in that number;
a double free is a crash on the Linux job. Rule 12 of the PLAN exists because
that difference is not symmetric.

### What it costs at run time today: nothing

A record, a user enum and a fixed array **release nothing**
(`own.rs:335`, `Type::Record(_) | Type::Enum(_) | Type::ArrayN(..)` → no row).
So a hole in a record has no runtime consequence at all today: the field's word
stays in the slot, nothing walks it, and nothing frees it twice. The take is the
load the copy already emits, without the `deep_copy` that follows it
(`vyrn-codegen/src/lib.rs:9545-9558`).

Read back through the compiler at `f7a37e8`:

```text
line 8     d                NOT reclaimed — nothing releases the type Doc yet
```

**This is the same shape as `for x in consume xs`, and the loop already shows
where the wiring goes.** `Stmt::ForIn`'s `consuming` flag reaches no engine, but
the loop is not invisible to `own.rs`: `movecheck` writes a `Gone::Moved` row
(`movecheck.rs:2032`), `own.rs` imports `Gone` (`own.rs:46`) and turns it into
`Fate::Moved` (`own.rs:895`), and the drop is not emitted. Checked:

```text
line 6     xs               moved at line 8 into the `for .. in consume` loop
```

So **M1 needs nothing new in `own.rs`**: a take of a whole binding writes the
same `Gone::Moved` the loop writes, through the same two lines.

**M2 is the one that adds something, and the bill arrives with RFC-0092 M3.**
`Fate` is one answer per binding and has no partial arm. Today that costs
nothing, because the record's answer is already *"nothing releases the type
`Doc` yet"*. When M3 gives `Record` and `Enum` a `Deep` release, that answer
becomes a walk, and the walk must skip the hole. See "What M3 must do" below.
M3 is unbuilt, and this RFC states the obligation rather than leaving it to be
discovered.

---

## Is it one mechanism or three?

**It is one, and the count says so before the argument does.** Of the 22
takeable sites: **20 are a record field, 2 are a container element, 0 are an enum
payload.** Saying that early is what made RFC-0092's milestones honest, and the
same is true here.

The dividing line is whether the container can represent the hole at run time.

| the place | can it hold a hole? | the answer | A sites |
|---|---|---|---|
| a container element | **yes** — a container has a runtime length | `swapRemove`, RFC-0011, shipped | 2 |
| a record field | no — a record has a fixed set of fields and no runtime mark | the new rule | 20 |
| an enum payload | no — an enum has a tag for the variant, not for the payload | already owned where it matters | 0 |

**The element case is closed and this RFC does not touch it.** `swapRemove`
returns the element and leaves the container one shorter, which is a hole a
container can represent. RFC-0092 M1's own migration proves the primitive is
sufficient: the synthesized `fromJson` decoder could not copy at all — a
self-referring target refuses `.copy()` — and it writes `f0.swapRemove(0)`. It
compiles, it allocates nothing, and it needed no new syntax. A hypothetical
`consume xs[i]` would have to invent a hole semantics that `swapRemove` already
answers, and would then have two spellings for one operation.

**The enum payload is zero and it is not a milestone.** A `match` over a fresh
call result already binds owners: `pattern_binders` asks
`place_path(scrutinee)` (`movecheck.rs:1153`) and a call is not a place, so the
binders are owned today. Every payload copy left in the corpus matches on a
borrowed parameter or on `self`, which no take rule reaches. The design's first
draft made this a milestone of its own — the easy half, `path == root`, no hole
rule — and the count deleted it.

**The whole-binding form still ships, as a form and not as a milestone.** It
costs one line, because a `consume` prefix is neither a `Var` nor a `Field` and
`place_path` therefore answers `None` for it. It earns its place at one site:
`examples/graphql.vyrn:195` matches an **owned local** `Option<Response>`, so
the binder is a projection, and `match consume r { Some(res) => consume res.body }`
is what makes it writable. One site is a reason to include a form and not a
reason to gate a milestone on it.

So the chain is: **the prefix is the mechanism; the record field is the case that
needs the hole and holds 20 of the 22 sites; the enum payload holds none; the
element is already done.** There is no honest place to stop before the hole.

---

## What it buys, counted

**44 of the 114 copies PR #109 added, and 44 of the 470 hand-written copies in
the corpus.** The table is in "The count" above. Three things about that number
are worth stating separately, because each of them could have gone the other way.

**All 22 A sites were added by PR #109.** The 383 copies that existed before it
contain **zero** takeable sites. RFC-0092 M1 did not expose a latent class; it
created one. That is the strongest single argument that the copies are the
symptom and this is the cause — nobody was writing this shape until the rule made
them.

**The design choice is worth 22 copies on its own.** A whole-root take gets 22
and a path-keyed hole gets 44. The 22 in between are the same programs, one field
later.

**And it is worth four public signatures.** `vyxParseElem`, `vyxProcessElem`,
`gqlArgList` and `gqlSelSet` each grew a `modify Array<..>` out-parameter and lost
their return value, because there was no way for the caller to read one field out
of a returned pair. Those four are not in any copy count. They are the cost that
does not show up as a copy, and they are why the seven shape changes are in this
RFC's evidence at all.

---

## Worked examples

Seven of the eight are the real shape changes PR #109 landed, read back through
the rule. The eighth is not a shape change and it is the one that decides the
design.

### 1. `std/vyx` — the pair that had to become an out-parameter

`vyxParseElem` returned `VyxNodeR { node, next, err }`, and the caller wrote
`nodes.push(er.node)`. RFC-0092 refuses that: `er.node` is a projection of a
record the frame owns. A `.copy()` is not available — `VyxNode` refers to itself
— so `.copy()` would have been a deep subtree copy at every `v-if` branch even
if it were writable. M1's answer was to delete the field and pass the caller's
list in:

```vyrn
type VyxNodeR = { next: Int64, err: String }

fn vyxParseElem(ba: Array<UInt8>, i: Int64, e: Int64, fileId: String, out: modify Array<VyxNode>) -> VyxNodeR
```

With the take, the pair comes back and the caller reads both halves:

```vyrn
let er = vyxParseElem(ba, j, e, fileId)
if er.err != "" {
    return VyxParse { nodes: nodes, next: er.next, stop: "", err: consume er.err }
}
nodes.push(consume er.node)
j = er.next
```

Two takes on one record, and `er.next` is read after both. This is the example
that refuses answer (a): a whole-root move makes the last line a use-after-move.

### 2. `std/graphql` — the same shape, twice

`gqlArgList` returned `GqlArgs { args, err }` and `gqlSelSet` returned
`GqlSet { sels, err }`. Both became `-> String` with a `modify Array<..>`
out-parameter. With the take, the return type is a value again:

```vyrn
let parsed = gqlArgList(sc)
if parsed.err != "" {
    return GqlSel { name: "", alias: "", args: [], sels: [], err: consume parsed.err }
}
args = consume parsed.args
```

### 3. `std/vyx` — `vyxProcessElem`

Same as 1, one level up: the node is handed back and the errors are appended to
the caller's list. The take returns it to a `{ node, errs }` pair.

### 4 and 5. `std/http` — `httpApply` and `httpMaybe404`

**The take does not undo these, and the RFC says so.** They rebuilt a `Response`
from the fields of an owned one:

```vyrn
return Response {
    status: httpStatus(r, answered.status, loc),
    contentType: answered.contentType,
    body: answered.body,
    vary: varyOn,
    headers: hs,
}
```

M1 rewrote them to edit the response in place — `answered.status = 304` and so on
— which reads better and copies nothing. A take would make the rebuild legal
(`body: consume answered.body`), but the rebuild was never the better program.
The one copy left in `httpApply` is `answered.vary = r.varyOn.copy()`, and its
root `r` is a `read` parameter, so no rule in this RFC reaches it. **These two
are a shape change that was an improvement, not a workaround, and they stay.**

### 6. `vlog`'s `Read` carrier

Deleted, because `readLine` already answers an `Option` and RFC-0060's `if let`
reads one. A take would have kept the carrier alive. **Deleting it was right**,
and the take does not argue for bringing it back. `vlog`'s other carrier,
`Decoded`, pays a copy and the take removes that one.

### 7. The synthesized `fromJson` decoder

Already takes: `f0.swapRemove(0)`. Nothing to change, and it is the evidence that
the element case needs no new mechanism.

### 8. `std/vyx` — the nine-line drain, and why the hole rule is not optional

Not one of the seven. It is the site the count keeps pointing at
(`std/vyx.vyrn:1431-1439`). One record is built by a call and immediately emptied
into nine locals:

```vyrn
let sc = vyxParseScriptAt(ba, scriptSec.start, scriptSec.end, dir, fileId)
if sc.err != "" {
    return vyxCompErr(compName, fnName, fileId, srcPath, sc.err)
}
propNames = sc.propNames.copy()
propTypes = sc.propTypes.copy()
imports = sc.imports.copy()
importLines = sc.importLines.copy()
importCols = sc.importCols.copy()
helpers = sc.helpers.copy()
helperTexts = sc.helperTexts.copy()
helperLines = sc.helperLines.copy()
helperCols = sc.helperCols.copy()
```

Nine array copies, and `sc` is dead on the next line. **Under a whole-root take
only the last of the nine is legal**, because the first eight are followed by a
read of `sc`. Under the path-keyed hole all nine are `consume sc.propNames` and
so on, and the record is empty rather than moved.

This is the site that decides between answers (a) and (c), and it is why the
count is 44 rather than 22.

---

## What this does not close

- **A hole the compiler cannot see statically.** A take inside one arm of an `if`
  leaks the field on the other arm. That is the union rule, deliberately, and the
  alternative is a runtime drop flag that RFC-0089 rule 5 forbids.
- **A take from a borrowed parameter.** Unchanged: the frame does not own it.
  `.copy()` stays the answer and `consume` on the parameter stays the other one.
- **A take from module state.** Unchanged and deliberately so.
- **A copy walk.** `impl Copy for Json` copies because it is asked to. About a
  dozen of PR #109's added copies are copy-walk bodies and no take reaches them.
- **A container element.** `swapRemove` answers it. This RFC adds no
  `consume xs[i]`.
- **RFC-0092 M2, M3 and M4.** Untouched, except for the obligation M3 inherits,
  below.
- **The seven deferred returns.** They are `return match hit { Some(r) => r, .. }`
  on an owned `Option<Response>`. A take spells them
  `return match consume hit { Some(r) => r, .. }`, which is legal under this RFC
  and free — but they are not refused today, so nothing forces the edit until M3
  refuses them. M3 still owns them.

---

## What RFC-0092 M3 must do about this

M3 gives `Type::Record`, `Type::Enum` and `Type::ArrayN` a `Deep` release. One
thing must hold that does not hold today, and it does not hold today only
because no record release exists today.

**A drop of a record must skip the taken fields.** The set is static: it is the
union hole set at that drop site, which `movecheck` computes and hands to
`own.rs` beside the `Gone` map it already hands over. That is one set per
binding and not a runtime flag.

**`Fate` needs a partial arm to carry it.** Today the answers are `Reclaimed`,
`Moved`, `Dropped` and `Leaked` (`own.rs:883-895`), each of them one verdict for
one whole binding. A record with a hole is "reclaimed, minus these fields", and
that is the one new shape. It is small, and M3 is the change that has to look at
`release_kind` for a record anyway.

**Nothing else is owed.** The obvious worry — that `for x in consume xs` would
double-free once a container releases its elements — does not hold: the loop
already writes `Gone::Moved` and `own.rs` already suppresses the drop. That was
checked rather than assumed, and it is why this section is one item and not
three.

---

## What it costs in the three engines

### The frontend

- **Parser.** One prefix position for `consume`, contextual by the test that
  already exists. `Expr::Consume { place, line }`.
- **`movecheck`.** `place_path` and `element_path` answer `None` for
  `Expr::Consume`, so a taken value is an owner at every store, every return and
  every pattern position, with no second rule. `check_consuming_iter` loses its
  `path != root` branch and gains a caller. `Consumed` becomes path-keyed, and a
  use of `p` after a take of `p.f` is checked by prefix. That last one is the
  only new logic in the pass.
- **`checker`.** `consume p` has the type of `p`. Nothing else.
- **`fmt`.** One token, printed with one space after it. RFC-0017's safety
  invariant is re-lex equality, so the gate is the existing one.
- **LSP.** `semanticTokenScopes` gains the third position of a keyword it already
  highlights.

### The interpreter

A `Val` is `Rc`-shared. A take clones the `Rc` and marks the slot. Nothing is
released, so nothing is observable — which is the point: **parity is the gate and
the expected result is byte-identical output**, exactly as it was for the
consuming loop.

### The textual backend and the direct wasm backend

**Nothing, until RFC-0092 M3.** `consume d.title` lowers to the load that
`d.title` already lowers to. No new emission shape, no bytes in
`domdemo.wasm`. The take is the absence of a copy call, so the emitted output
is strictly smaller at every migrated site.

---

## Does it disturb RFC-0092's rule?

No, and the reason is structural rather than argued.

RFC-0092's rule is *a projection is a borrow of its root, whatever the root is*.
A take does not produce a projection. `consume d.title` is not a `Field`
expression; it is a move, and `store` sees an owned value at the same place it
would have seen a projection. **The rule is not weakened, narrowed or
conditioned** — the program stopped writing a projection there.

M1's instrument is the proof and it is already in the tree. It asserts zero
projection stores, zero element stores and zero element returns
(`cargo test -p vyrn-frontend --lib borrow_store_sites -- --ignored`). A take
site is not a projection store, so the zero stays zero and a regression still
fails. **The instrument does not need a second mode for this RFC**, and if a
milestone finds it does, that is a signal the take leaked into the projection
path and the milestone should stop.

---

## Milestones

**There is no gate phase, and the reason is that the gate already ran.** Every
other RFC in this chain opened with a measurement milestone because the corpus
had not been read. This one opens after RFC-0092 M1 migrated it, so the count is
in "The count" above and the decision it gates is made below.

**There is also no separate `path == root` milestone, and that is the count
talking.** The design's first draft had one: the whole-binding prefix, the enum
payload half, no hole rule. The census says the enum payload is **zero copies**,
because a `match` over a fresh call result already binds owners. A milestone that
buys nothing is not a milestone. The whole-binding form still ships — it is what
makes `match consume r { Some(res) => consume res.body }` writable at
`examples/graphql.vyrn:195`, where the payload binder is a projection of an owned
local — but it ships **inside** M1 as a form, not as a milestone with its own
gate.

### M1 — the prefix and the hole

`consume p` as a prefix on any place, in every expression position: scrutinee,
argument, store, return, `let`. Path-keyed `Consumed`, revival on reassignment,
union at joins. `check_consuming_iter`'s `path != root` refusal is deleted, so
`for t in consume b.tags` becomes legal in the same change — and the menu that
sends a record root to `for .. in consume b`, which does not typecheck, goes with
it, because it no longer has anything to say.

Deliverables: the parse, `place_path` answering `None` for `Expr::Consume`,
`check_take` (the existing `check_consuming_iter` minus one branch), path-keyed
consumption, `fmt`, the LSP token, and the diagnostics for the three refusals
that stay.

The corpus migrates in the same change, module by module, as every phase in this
chain has done.

**Gate, and it has two halves.**

*Correctness.* Three-way parity byte-identical including traps. The memory suite
unmoved — eleven steady of twelve with `elementLeak` still leaking. The
`borrow_store_sites` instrument still zero, without a second mode. **M1 changes
what a program may say and must move no row.**

*The count.* The migration must remove **at least 40 `.copy()` calls**, against
the 44 measured. **Below 30, stop and write the number**, and the RFC's "as
landed" section says which sites refused to convert and why. That is the gate
that stopped RFC-0082 M2 three times and refused RFC-0091 M4, and it is meant to
be able to stop this.

### M2 — the release, jointly with RFC-0092 M3 — **LANDED**

Not a milestone of this RFC alone, and it does not start until RFC-0092 M3 does.
The hole set travels from `movecheck` to `own.rs` beside the `Gone` map it
already hands over, `Fate` gains its partial arm, and M3's record release skips
the hole.

**Gate.** RFC-0092 M3's own gate — `elementLeak` flips to steady — plus one new
memory row: a record whose `String` field is taken in a loop of N iterations
allocates N and frees N. Not 2N, which is the double free, and not 0, which is
the leak this RFC ships with until M2.

---

## Rejected

- **A new word, `take`.** Two words for one meaning. RFC-0089's vocabulary is
  four capabilities and `consume` is the one that means ownership transfers here.
  PR #74's own summary named reuse as the decision it made deliberately.
- **A method, `d.title.take()`.** It would follow `swapRemove`'s shape — a
  builtin that needs the binding rather than the value — and the parser already
  handles that class. Refused for two reasons. A method call reads as an
  operation on a **value** and this is an operation on a **place**; the parser
  already refuses `consume self` on a `place` for the mirror-image reason
  (`parser.rs:2084`). And it would give the language two spellings for one
  meaning, since `for x in consume xs` is not going to become
  `for x in xs.take()`.
- **A move of the whole root.** Answer (a) above. Two lines to build and it does
  not fix the corpus, because the sites read another field afterwards.
- **A runtime drop flag.** Answer (b). RFC-0089 rule 5 says `Ref<T>` is the one
  runtime mechanism and the only one.
- **`consume xs[i]` for a container element.** `swapRemove` already returns the
  element and leaves a shorter container, which is the hole a container can
  represent. A second spelling would be the two-spellings defect this chain
  spends its time removing.
- **Refusing a take where the hole is not statically known**, instead of leaking.
  Refusing is stricter and it costs a copy at every conditional take. The leak is
  the direction the compiler already fails in — 2,127 bindings not reclaimed —
  and it is the safe direction. If the corpus turns out to want conditional takes
  often, the answer to revisit is a **dominance** requirement (the take must
  dominate the end of the scope), not a flag.
- **Making `.copy()` cheaper instead.** M1b measured a String copy at 25 ns, one
  `malloc` and one `memcpy`. The copies this RFC removes are not slow; they are
  **wrong**, in the sense that the program did not want two owners. The seven
  shape changes are the evidence: each one is a program bent around a missing
  verb.

---

## The recommendation

**Build M1. Hold M2 until RFC-0092 M3 starts.**

The reasons, in order of weight.

1. **It is not a keyword.** The brief for this design asked whether the count
   justifies a keyword. It does not have to: `consume` is an existing word at a
   third position, which is the decision PR #74 made deliberately for the second
   position and wrote down as the reason it cost so little. The question this
   count has to answer is smaller — whether 44 copies and four distorted
   signatures justify **one prefix, one deleted refusal, and a path-keyed map**.
   They do.
2. **44 of 114, and every one of them created by the previous milestone.** The
   383 copies that predate RFC-0092 M1 contain zero takeable sites. This is not a
   pre-existing sloppiness the language could keep tolerating; it is a class M1
   opened, and PR #109 filed it rather than forcing it.
3. **It costs the two compiling backends nothing and the interpreter nothing.**
   M1 is a frontend change, exactly as `for x in consume xs` was, and for the same
   reason: nothing releases a record's fields yet. Parity's expected result is
   byte-identical output.
4. **It does not reopen RFC-0092.** A take is not a projection, so the rule M1
   just turned on is neither weakened nor conditioned, and the instrument that
   guards it needs no second mode.
5. **It removes a diagnostic that sends the reader to a second error.** The
   consuming menu offers `for .. in consume b` for a record root, which does not
   typecheck. That is this chain's own defect — one question, two answers — and
   the take is what lets the menu stop saying it.

**The gate that changes the answer is M1's own migration count.** If the
migration removes fewer than 30 copies, the design was wrong about the corpus and
the right response is to keep the number, keep the four out-parameters, and close
this RFC with the measurement attached. The most likely way that happens is a
site where the take is legal but the reviewer prefers the copy, and the honest
place to find that out is the migration rather than this document.

**And one thing this RFC is deliberately not recommending.** 183 of the corpus's
470 copies — 39% — are projections of a **borrowed** parameter, and no rule here
moves any of them. `std/http`'s `httpCopy` and `std/ui`'s `Head` builders are the
two largest clusters of copies in the whole corpus, and the only thing that would
move them is `consume` on the signature, which is a decision about every caller.
That is a different question and it deserves its own count, not this one's.

---

## M1 as landed

**The count is 45, against a prediction of 44 and a gate of 40.** The corpus
holds 454 `.copy()` calls where it held 499. Every site was classified by the
compiler and not by hand: apply the take, run `vyrn check`, keep what
`check_take` accepts. The extra copies over the predicted 44 sit inside the four
restored signatures — `gqlArgList`'s `return v.err.copy()` is one — so they are
copies the shape change removed rather than copies replaced in place.

**The nine-line drain takes all nine.** `std/vyx.vyrn:1431-1439` is nine
`consume sc.<field>` lines with `sc` never read as a whole afterwards. That is
the site answer (a) could not have: under a whole-root move only the ninth is
legal. The design's central choice survived its own test.

**The four signatures are back.** `vyxParseElem` and `vyxProcessElem` return
their `{ node, .. }` pairs; `gqlArgList` and `gqlSelSet` return `GqlArgs` and
`GqlSet`. Each caller writes `consume p.node` or `consume parsed.args`. One thing
was worse than "a signature": `vyxProcessElem`'s distortion had also turned a
running error `String` into a caller's `Array<String>`, because the out-parameter
had nowhere else to put it. Restoring the pair restored the simpler shape too.

**The whole-binding form earned its one site**, exactly where the count said it
would. `examples/graphql.vyrn:195` is now
`match consume r { Some(res) => consume res.body }`.

### The two things the design got wrong

**A `[` is not a path boundary, and the take's relation must say so.** The RFC
said `root_of` already exists to compare a path with its root. It does, and it
splits on `.` **or** `[` — which is right for a root and wrong for this.
RFC-0082's place desugar names its temporaries after the paths they took, so
`o.i.xs[k] = v` moves through bindings literally called `o.i[]` and `o.i[].xs[]`.
Read as paths, the second is inside the first, and every write-back then reads as
a use of something moved. They are not paths; they are one binding each. `under`
therefore relates fields only, and a name carrying a `[` relates to nothing but
itself. `tests/places.rs` caught this twice, once per direction.

**A hole is a flag, not a shape.** "The root has a hole in it" cannot be decided
by testing whether some consumed key is longer than the path — for the same
desugar reason. `Consumption` carries a `hole` bit, set only by a take of a
projection. Everything else in the map is a whole binding moved whole, whatever
its name looks like.

### What that changed in the plan, and what it did not

**The loop is path-keyed too.** The RFC described the prefix and left
`for t in consume b.tags` as a relaxation. It is more than that: the loop must
record `b.tags` rather than `b`, or the field it took empties the whole record.
One `if root == path` around the `took` call, and the same `hole` bit.

**A place chain asks ONE question, of the whole path.** The RFC's "a use of `p`
after a take of `p.f` is checked by prefix" is half the rule. The other half is
that a use of `p.g` after a take of `p.f` must not be checked at `p` at all, so
`Expr::Field` stops recursing into its root for the consumption question and asks
it once, of the path it names. The root still gets its capture bookkeeping.

**Everything else held.** `own.rs` needed nothing. The textual backend and the
direct wasm backend needed one recursion arm each, and three-way parity is
byte-identical — 124 checked, 11 skipped, 0 failed. The memory suite is unmoved:
eleven steady, `elementLeak` leaking. RFC-0092's instrument reads `stores: 0`,
`elem-store: 0`, `elem-return: 0` and `returns: 7`, without a second mode — a
take is not a projection, so the zero stayed zero on its own.

### Diagnostics, which were half the deliverable

The refusal that RFC-0092 M1 could only answer with `.copy()` names the take
first now, and only where `check_take` would accept it — an owned root, not a
borrowed one and not module state:

```text
`b.name` may not be stored into `push(..)` — it is read out of a place that owns it
  fix: `consume b.name` if `b` should give it up — the field is dead afterwards
  fix: `b.name.copy()` if both sides need a value
```

Using the root after a take names the take's line, because that is the line the
reader has to change:

```text
`b.name` was taken out of `b` here
line 22: ... and `b` is used as a whole here, with the hole still in it
  fix: `b.name.copy()` on line 21 if `b` is still needed whole
  fix: write `b.name` back before this line
```

And `consume xs[0]` no longer says "nothing to take". A container is the one
place that can hold a hole at run time, so the menu names the primitive that
already spells it:

```text
`xs[0]` may not be taken — an element is not a place a take reaches
  fix: `xs.swapRemove(..)` returns the element and leaves the container one shorter
```

### What M1 did not close

Everything under "What this does not close" above still stands. In particular the
**183 C-bucket copies** off a borrowed parameter or `self` are untouched, and
`std/http`'s `httpCopy` and `std/ui`'s five `Head` builders are still the two
largest clusters in the corpus. They want `consume` on the signature, which is a
decision about every caller and deserves its own count.

---

## M2 as landed

**The hole set travels, and the walk skips it.** `movecheck` accumulates every
take of a projection of one root into `Gone::Hole { line, paths, skippable }`,
`own` turns it into `Fate::Reclaimed(kind, paths)` and a `holes` map keyed the
way `droppable` is keyed, and both release walks — `deep_release` in the textual
backend, `rel_at` in the direct one — carry the set down and skip a place whose
path is in it. The paths are relative to the binding (`title`, `head.err`), so
one hop of the walk strips one hop of the path.

**The carrier is a second map, not a field on `DropKind` and not `Fate` alone.**
The brief asked for the smaller diff of the two it named and this is a third: a
`DropKind` answers for a TYPE, and every construction site of one — including the
sites that answer for a store and for an explicit `drop` — would have had to
carry an empty set. `Fate::Reclaimed` does carry it, because the report prints it
(`reclaimed at block exit — releasing what the Doc holds, except `title`, which a
`consume` took`), but a backend reads `droppable` rather than `notes`, so the map
is what reaches the walk. Five edit sites for `Fate`, one new map, and no
`DropKind` assertion moved.

### Where a hole still leaks, and there are three

Each of these is a place the walk cannot be told about, and the direction of
failure is fixed: leak.

- **A declared `impl Owned for T` release.** It is a user function, and a
  function cannot be told to leave one field alone.
- **A path that is not a chain of record fields.** An enum's live variant is a
  runtime tag, so a hole under a payload is not a place a static walk reaches.
  Unreachable today — M1 refuses a take of a payload and of an element — so it is
  a guard for the rule rather than for the corpus.
- **A write that fills the hole.** `d.title = v` after `consume d.title` revives
  the place, and the store that revives it releases what the place held, which is
  the buffer the take gave away. So the binding leaks whole. The program compiles
  today and leaked under M1, and this is the one case where M2 could have
  introduced a double free rather than removed one.

### The thing the design got wrong, and genwasm found it

**A hole is not the last word.** The first build made `Gone::Hole` behave like
every other `Gone` row — written once, never overwritten — and that is wrong in
one direction. Every other row says the value **left**: moved, returned,
captured, lent. Those must win, and `gave_up` marks the root of every name a
`return` expression reads, so `return VyxParse { .., err: consume er.err }`
records that `er` left. Keeping the hole there released a value the caller now
held.

**Three-way parity passed with that bug in it — 124 checked, 0 failed.** The
wasm generator engine did not: `vyrn emit-gen examples/vyxdemo.vyrn` trapped
inside `std/vyx`, because a generator runs as compiled wasm and reads its own
freed memory. That is the second time in this chain that `genwasm` has caught
what parity could not (Phase 10a was the first), and it is why the gate lists it.

The fix is one condition in `took`: a `Gone::Hole` row is overwritable. Every row
that overwrites it suppresses the release, so the change can only leak.

### The numbers

- **Three-way parity: 124 checked, 11 skipped, 0 failed**, byte-identical
  including traps.
- **The workspace: 52 test binaries, 1,513 tests, 0 failures**, with
  `--no-fail-fast`. (One run before the fix showed the port-flaky `serve` test;
  it passes alone and passed in the final run.)
- **`genwasm`: 11 passed, 0 failed.**
- **The memory suite: 15 rows, 14 steady, `keysLoop` still the only leak.** The
  new row is `takenField` — a record whose `String` field is taken in a loop.
  It is a real instrument and was checked as one: with the skip disabled it reads
  2,162,688 bytes after 500 calls and 8,388,608 after 2,000, and with the skip it
  reads 131,072 both times. Every other row is unmoved.
- **RFC-0092's instrument is unmoved**: `stores: 0`, `elem-store: 0`,
  `elem-return: 0`, `returns: 0`, without a second mode.

### The corpus count, and the brief's number was wrong

The brief said `vyrn why --memory` reads 2,274 bindings not reclaimed and M2 must
bring it to 2,127 or below. **It reads 1,972 before this change and 1,958 after**
— measured over the same 200 files that answer without a project, ten of which do
not, which is the measurement that reproduces RFC-0092 M1's 2,127 exactly at
`23aa9cf`. The chain reads:

| commit | milestone | not reclaimed |
|---|---|---|
| `23aa9cf` | RFC-0092 M1 | 2,127 |
| `75388a6` | RFC-0093 M1 | 2,129 |
| `ee18590` | RFC-0092 M2 + M3 | 1,972 |
| this | RFC-0093 M2 | **1,958** |

**RFC-0092 M3's "2,274, up from M1's 2,127" does not reproduce.** The take costs
**two** rows, not 147: M1 left 19 bindings carrying a hole, and 17 of them were
already leaking for another reason before the take existed.

**Zero hole rows are left in the corpus.** Of the 19, two are now reclaimed minus
their holes — `examples/fieldmut.vyrn`'s `names` and `std/vyx.vyrn`'s `imp`,
which has three — twelve are `moved into the return` (the `gave_up` sweep, which
marks the root of every name a return reads and is a leak in the safe direction),
and five are `it is a value its producer does not own`, because a hole row no
longer blocks the lender rule that runs after the walk.

**So the mechanism buys two corpus bindings today, and that is the honest
number.** What it buys is not the count: it is that a drained record has a
release rule at all, so the 45 takes M1 landed no longer each cost a binding.
