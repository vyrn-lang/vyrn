# RFC-0120 — a result carries a capability

- **Status:** Implemented (2026-08-29): `-> read T` / `-> modify T` parse on
  impl members and classify into the projection table all three engines
  already inline from; `place`/`yield` removed (the retired spelling gets
  its own parse error naming the replacement); any method name dispatches a
  projection in the checker, the interpreter, the shared lowering and both
  backends; movecheck's element-read rules extend to named projections by
  name. Witness `examples/namedplace.vyrn`: three-way identical output, and
  its two benches price the same label read at 3 ns through the projection
  against 24 ns as an owned copy.
- **Evidence:** the std-quality census's pattern 2 (ten modules whose hot
  reads copy), RFC-0109's open question, and a session of measurement: an
  in-place read costs 1–9 ns where the copy it replaces costs 47 ns
  (`slots`), 201.6 µs (`jsondec`, one 4096-element lookup) or 490 µs
  (RFC-0109's 40 KB record). The mechanism that closes the gap has existed
  since RFC-0091 — gated behind three names and two borrowed keywords.

## Why a copy cannot simply be deleted

Vyrn's ownership is defined, not inferred (RFC-0087): every heap value has a
free point the source determines. Suppose `let s = xs[0]` handed out a
pointer instead of a copy, with no other mechanism:

```vyrn
let mut xs: Array<String> = []
xs.push("hello world, a heap string")
let s = xs[0]     // a pointer into xs's storage?
xs = []           // the storage is freed — defined ownership demands it
print("{s.byteLength}")   // use-after-free
```

This program forces the choice. To keep it safe a language must do exactly
one of: copy at line 3 (today), forbid line 4 while `s` lives (a
restriction), delay the free until `s` dies (counting or tracing), or stop
being memory-safe. There is no fifth behavior.

Nor can a compiler silently pick "pointer where provably safe, copy
elsewhere" and be exact about it: whether an alias outlives its storage is a
semantic property of the program — undecidable by Rice's theorem (route the
alias through `stash = s` behind an arbitrary computation and deciding
safety decides that computation). Every sound scheme therefore gets its
evidence from one of exactly three sources — a decidable syntactic fragment,
programmer annotations, or the runtime — or makes the question vacuous by
copying. Vyrn already holds the cheapest cell of that partition: the
scope-nested fragment, where an alias provably dies inside the expression
that created it because it *syntactically cannot leave*. That fragment is
RFC-0091's projections. This RFC finishes them.

## The two gates, and the two keywords

A projection today is spelled

```vyrn
place at(read self, i: Int64) -> T {
    yield self.vals[i]
}
```

and is reachable through exactly three names: `at` (behind `a[i]`), `atSet`
(behind `a[i] = v`), `nth` (behind `for`). A method named anything else is
not consulted — `jsondec`'s `fieldAt`, the census's whole pattern-2 list,
copy their results at 10³–10⁵ times the in-place price while the machinery
that would serve them sits one lookup away (`project::site` is already
name-agnostic; the sugar desugars are its only callers).

Both keywords are borrowed jargon. `place` is compiler-internals vocabulary
(a "place expression" is an lvalue). `yield` is Swift's accessor-coroutine
word, where statements after the yield perform write-back — an ability
Vyrn's projections do not have and its parser refuses (`yield` is the exit;
`return` is a parse error inside one). Two new words, each signaling
something the feature isn't.

## The design: spell the projection in the signature

The capability column already exists on parameters — `read self`,
`modify self`, `consume x`. This RFC puts it on the result:

```vyrn
fn at(read self, i: Int64) -> read T {
    return self.vals[i]
}

fn atSet(modify self, i: Int64) -> modify T {
    return self.vals[i]
}
```

`-> T` gives the caller its own T — ownership, today's meaning, unchanged.
`-> read T` gives the caller permission to look at a T the receiver keeps.
`-> modify T` gives permission to write through. Read aloud, the signature
is the contract; no new words, `return` means what it means everywhere else.

The marking convention is the same one parameters already follow: the
unmarked form is the position's universal case. A parameter always has a
surviving owner (the caller), so unmarked means `read`. A result usually has
none — `return a + b` constructs a value nobody owns after the call, so
owned is the only capability every result can support, and unmarked means
owned. `-> consume T` is refused with a message: it is spelled `-> T`.

Rules, all pre-existing (RFC-0089 rule 2, RFC-0091 M2):

- The body is statements followed by a final `return <place>`, where the
  place is a field/element chain rooted in `self`. Early `return` is
  refused, as it is today.
- A capability result exists on impl members only. A protocol declaring one
  is refused with its own sentence — protocols keep owned-result methods,
  and the projection lives on the impl, exactly where the retired spelling
  put it. Letting a protocol *require* a projection is this RFC's recorded
  gap; nothing in the corpus asks for it yet.
- The access site inlines the body — a projection is never called, so the
  alias cannot outlive the access *by construction*, and `own`, `movecheck`
  and the lowering see element places they already govern. No new check.
- A capability result on a free function is refused: a projection needs a
  receiver that outlives the access, and a free function's result has none.

### Dispatch is un-gated

`x.f(i)` where `f` names a projection on `x`'s type inlines exactly as
`x[i]` inlines `at` — same `project::site` expansion, same memo, same
addresses. `a[i]`, `a[i] = v` and `for` keep desugaring to `at`, `atSet`,
`nth`; the three names stop being the only door.

### The old spelling is removed

`place` and `yield` leave the grammar. Pre-1.0, the repo is the user base:
three std projections (`slots` ×3, `stream` ×1), five examples, the inline
test programs, and the docs pages that show them. `vyrn fmt`, the LSP's
completion/hover/templates, and the reserved-word census follow.

## What this is not

- Not view types: an alias still cannot be stored in a `let`, a record, or
  module state. That is the next cell of the partition and it stays unbuilt
  until a profile names a workload that is scan-shaped rather than
  getter-shaped — the measured ones are all getters.
- Not inference: the fragment stays keyword-marked *in the signature*
  because silent inference lets a one-line edit to a callee flip every call
  site from a 1 ns alias to a 490 µs copy with no diff at any call site — a
  5×10⁵ cost change the signature exists to make a compile error instead.
- Not a runtime: nothing is counted, nothing is checked at access time
  beyond the bounds checks the body already writes.

## Milestones

- **M1 — the spelling.** `-> read T` / `-> modify T` parse on impl members
  and protocol declarations and classify into the projection bucket the
  engines already read; `place`/`yield` removed; every in-repo user
  migrated; `vyrn fmt` and the LSP follow; `-> consume T` and early
  `return` and free-function capability results get their refusals, each
  with a witness.
- **M2 — the dispatch.** `x.f(args)` consults projections in the checker,
  the interpreter, the lowering, and both backends, through the shared
  `site` expansion. Witness: a named projection reads through an enum
  payload via `if let` at the in-place price, three-way parity.
- **M3 — the adoption.** Measured in `examples/namedplace.vyrn` (3 ns vs
  24 ns, an 8x on a one-line label; the ratio grows with the value). The
  honest finding of the conversion pass: the census's pattern-2 rows are
  NOT all reachable from here, because each is blocked on a shape this RFC
  deliberately does not add — `slots.get` answers an `Option<T>` and an
  option of a place is a view question; `jsondec`'s accessors walk enum
  payloads, which needs a place to survive a `match`; `contract`'s copies
  sit behind free-function APIs, and a free function's result has no
  receiver. Un-gating was still the right first move — it is the mechanism
  those extensions would compose over, and it prices the getter shape at
  1–9 ns wherever an impl can host one — but the pattern-2 payers now name
  exactly which extension each one waits for, instead of "RFC-0109" in
  general.
