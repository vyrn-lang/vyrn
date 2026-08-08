# RFC-0093 — A Take Is a Move Out of a Place

- **Status:** **Designed, not built.** The count is measured. The recommendation
  is at the bottom and it is "build M1 and M2, and stop there until the gate at
  M3 is read."
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
(`compiler/vyrn-frontend/src/movecheck.rs:1190-1201`). It is the whole design in
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

Measured over the corpus at `f7a37e8`: **230 `.vyrn`/`.vyx` files, 448 `.copy()`
calls**, of which PR #109 added 114 and removed 7.

Split by the shape of what is copied, over the lines PR #109 **added**:

| shape of the copied expression | count |
|---|---|
| a field of a place — `er.err.copy()` | 54 |
| an element or a field of one — `xs[i].copy()`, `fs[0].key.copy()` | 22 |
| a plain name — `s.copy()` | 46 |

The 46 are not one thing. About thirty are a `match` arm binder — `JStr(s) =>
s.copy()` — which is an **enum payload** wearing a plain name, and the rest are
loop variables and whole locals. A dozen of the plain names are the bodies of
the new `impl Copy` walks: a copy walk must copy, and no take reaches it.

The count that decides this RFC is not "how many copies exist". It is **how many
copies have a root the frame owns and does not read again**, because only those
can become a take. That census is in "What it buys, counted" below.

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
   `consume` when `(` follows it (`parser.rs:2537`, `parser.rs:3634-3638`). A
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

> **`consume p` yields the value at place `p` as an owned value, and `p` is dead
> from that point.**

Three refusals, and all three are already written. `check_consuming_iter` is the
function; a take reuses it with one branch deleted.

| refusal | today's site | why it stays |
|---|---|---|
| `consume` on something that is not a place | `movecheck.rs:1180` | there is nothing to take from; the value is already owned |
| `consume` of a borrowed root | `movecheck.rs:1208` | the frame does not own it, so it may not give it away |
| `consume` of module state | `movecheck.rs:1202` | nothing may take module state, and that is true of every caller |
| ~~`consume` of a projection (`path != root`)~~ | `movecheck.rs:1190` | **this is the branch that goes** |

The fourth refusal is the gap. Deleting it makes `for t in consume b.tags`
legal, which is a relaxation of an existing error rather than a new form, and it
is what makes the expression prefix and the loop form agree.

### What happens to the place afterwards

Three answers were possible. The design takes the third and refuses the other
two for measured reasons.

**(a) The take is a move of the whole root.** `consume d.title` kills `d`.
Cheapest to implement — `took(&root, ..)` and `consumed.insert(root, ..)` are two
lines that already exist and are already called by the consuming loop
(`movecheck.rs:2025-2035`). **Refused, because it does not fix the corpus.** The
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
  `root_of` (`movecheck.rs:645`) already exists to compare the two.
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
stays in the slot, nothing walks it, and nothing frees it twice. The take is a
load, exactly as the copy's argument is a load before it calls `__vyrn_str_copy`.

Read back through the compiler at `f7a37e8`:

```text
line 8     d                NOT reclaimed — nothing releases the type Doc yet
```

**This is the same shape as `for x in consume xs`, and the loop already shows
where the wiring goes.** `Stmt::ForIn`'s `consuming` flag reaches no engine, but
the loop is not invisible to `own.rs`: `movecheck` writes
`Gone::Moved { line, by: "the `for .. in consume` loop" }`
(`movecheck.rs:2030`), `own.rs` imports `Gone` (`own.rs:46`) and turns it into
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

**It is two, and one of them already shipped.** Saying that early is what made
RFC-0092's milestones honest, and the same is true here.

The dividing line is whether the container can represent the hole at run time.

| the place | can it hold a hole? | the answer |
|---|---|---|
| a container element | **yes** — a container has a runtime length | `swapRemove`, RFC-0011, shipped |
| a record field | no — a record has a fixed set of fields and no runtime mark | the new rule |
| an enum payload | no — an enum has a tag for the variant, not for the payload | the new rule, at `path == root` |

**The element case is closed and this RFC does not touch it.** `swapRemove`
returns the element and leaves the container one shorter, which is a hole a
container can represent. RFC-0092 M1's own migration proves the primitive is
sufficient: the synthesized `fromJson` decoder could not copy at all — a
self-referring target refuses `.copy()` — and it writes `f0.swapRemove(0)`. It
compiles, it allocates nothing, and it needed no new syntax. A hypothetical
`consume xs[i]` would have to invent a hole semantics that `swapRemove` already
answers, and would then have two spellings for one operation.

**The record field and the enum payload are one mechanism, not two.** They differ
only in where the path stops:

- `consume d.title` has `path != root`. It leaves a hole, and the hole rule
  above governs it.
- `match consume e { Word(s) => s }` has `path == root`. It takes the whole enum,
  so there is no hole — the existing whole-variable move covers it, and the
  binders become owners for free. `movecheck.rs:1153` reads
  `place_path(scrutinee).map(|_| Borrow::Projection)`, and `place_path` answers
  `None` for anything that is not a `Var` or a `Field`. A `consume` prefix is
  neither, so the binders bind owners with no second rule. The same holds for
  `if let Some(x) = consume opt`.

So the chain is: **the prefix is the mechanism; the record field is the case that
needs the hole; the enum payload is the case that does not; the element is
already done.** A milestone can stop after the payload and still be honest,
because the payload case is the smaller half and it closes on its own.

---

## What it buys, counted

The corpus is migrated, so this is a measurement and not an estimate. Every
`.copy()` PR #109 added is in the tree.

<!--COUNT-->

---

## Worked examples

All seven are the real shape changes PR #109 landed, read back through the rule.

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

### M1 — the prefix, at `path == root`

`consume p` where `p` is a whole binding, in every expression position:
scrutinee, argument, store, return, `let`. This is the enum payload half, and it
needs **no hole rule at all** — it is the whole-variable move `movecheck` already
performs for the consuming loop, at a new position.

Deliverables: the parse, `place_path` answering `None`, `check_take` (the
existing `check_consuming_iter` minus one branch), `fmt`, and the diagnostics for
the three refusals that stay.

**Gate.** Three-way parity byte-identical including traps. The memory suite
unmoved — eleven steady of twelve with `elementLeak` still leaking. The
`borrow_store_sites` instrument still zero. **M1 changes what a program may say
and must move no row.**

### M2 — the hole, at `path != root`

`consume d.title`. Path-keyed `Consumed`, revival on reassignment, union at
joins. `check_consuming_iter`'s `path != root` refusal is deleted, so
`for t in consume b.tags` becomes legal in the same change — and the menu that
sends a record root to `for .. in consume b` goes with it, because it no longer
has anything to say.

The corpus migrates in this change, module by module, as every phase in this
chain has done.

**Gate.** The same three, plus: the number of `.copy()` calls in the corpus falls
by at least the A-count measured above, and no site converts to a take that the
census called B or C. If the migration finds fewer than that, **stop and write
the number** — that is the same gate RFC-0092 M0 used and it is the one that has
stopped work three times in this chain.

### M3 — the release, jointly with RFC-0092 M3

Not a milestone of this RFC alone. The hole set travels from `movecheck` to
`own.rs`, and RFC-0092 M3's record release skips it. **The consuming loop's
unpaid bill is fixed in the same change**, because it is the same mechanism.

**Gate.** RFC-0092 M3's gate — `elementLeak` flips to steady — plus a memory row
for a taken field: a record whose `String` field is taken N times allocates
N and frees N, not 2N and not 0.

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

<!--RECOMMENDATION-->
