# RFC-0109 — A Read That Does Not Copy

- **Status:** **Decided (2026-08-29): the stored view is a locator, not a
  pointer** — see "The decision" at the end. The trilemma there proves no
  freer design exists; the projection arc (RFC-0120..0123) already built the
  non-stored half; the stored half is source-plus-locator (a range, a path,
  a handle), which adds no heap edge and therefore no proof obligation. A
  dedicated `Range` type lands with its first payer; nothing ships today,
  and the re-measurement below is why that is the strong form of "free"
  rather than an evasion.

## The gap in one sentence

Vyrn has no way to read part of a `String` or an `Array` without copying it, so
every accessor in the standard library either copies or forces its caller to.

## Why the ownership words do not already cover this

Vyrn has four ownership words: `read`, `modify`, `consume`, `share`. A `read`
parameter does not copy the value it is given, and there is no type that names
"a view of someone else's bytes". A function that takes `read Scanner` and wants
to hand the scanner's source array back to its caller has one move available:
copy it.

That is the whole gap. It is not a missing optimisation. It is a missing type.

### Correction, 2026-08-25: it is ESCAPING that copies, not reading

The first version of this section said "reading a field out of a borrowed record
copies that field". **That is not true, and measuring it is what collapses the
option table below from four rows to one.**

Reading through a borrowed field is already free, on both engines:

| measurement | result |
| --- | --- |
| interpreter, 20,000 field reads in a loop | 0.48 µs each |
| interpreter, the same reads on the array directly | 0.21 µs each |
| native, 5,000 field reads over a 5,000-byte array | 108 ms |
| native, the same over 20,000 bytes | 91 ms |
| native, the same over 80,000 bytes | 92 ms |

Flat across a sixteenfold change in the size of the thing supposedly being
copied. The interpreter's 0.48 µs against 0.21 µs is one refcount bump, not
twenty thousand elements — `Val::Array` is an `Rc<Vec<Val>>` and has been since
the value-copy work; native reads the header in place.

What costs is a borrow LEAVING. Over a 40,000-byte array, 300 calls each:

| shape | per call |
| --- | --- |
| `lookInside(h)` — reads `h.data[0]` and `h.data.length` | **0.86 µs** |
| `takeOut(h)` — `return h.data.copy()` | **490 µs** |

Five hundred and seventy times, and the copy is not a choice. `return h.data`
does not compile:

```
`h.data` may not be returned — it is a `read` parameter, and a return is owned
  fix: declare the parameter `h: consume ..` if this function should own it
  fix: `h.data.copy()` if both sides need a value
```

So the gap is exactly: **a borrow cannot be returned or stored.** Which is the
question the last section of this document says it does not answer, and it turns
out to be the only question there is.

## The evidence, from three directions

Nothing below is estimated. Each line is a measurement or a file and line.

### 1. A hot loop, measured five ways

While making the site render faster, one substring matcher was rewritten five
times to find a shape that did not copy. Warm export, same work each time,
baseline 54.5 s:

| shape | result |
| --- | --- |
| the state in a record, passed `read` | 51.2 s |
| the record's fields hoisted into locals | 65.9 s |
| the fields passed as separate arguments | 57.4 s |
| dispatch through an empty-array sentinel | generator phase 140.4 s, worse than doing nothing |
| one extra call frame | +2.7 per cent |

Every shape that kept the state together paid a copy. Every shape that took the
state apart paid for taking it apart. There was no third option, and the search
ended by accepting the copy.

### 2. Ten standard library modules, found independently

A census of all 38 modules against thirty quality axes recorded "whole-value
copies where a read would do" as **the largest measured cost class**, in ten
modules, and reached the same conclusion without seeing the measurement above:

> std has no borrow-shaped or view-shaped read API for strings and arrays.

The ten, each with its own symptom: `jsondec` deep-copies per accessor level;
`slots` copies the payload on every `get`; `contract` copies the whole export
surface to answer one name query; `strings` copies per match in `split`; `hash`
copies the full padded input before hashing; `text` holds a bytes copy beside
the array in `chars`; `scan` copies the whole source in all three constructors;
`von` copies a String per token probe; `jsonread` allocates a byte array per
token and copies it twice; `vyx` calls `bytes()` inside a per-byte loop.

### 3. Three of the five benchmark gaps

A separate census timed the eight Benchmarks Game programs against their C
references. Vyrn is within four per cent on nbody, spectral-norm and pidigits.
Of the five programs that lose, three are attributed to `REPRESENTATION`, which
is this gap under another name:

| program | ratio | attribution |
| --- | --- | --- |
| fannkuch-redux | 1.41x | one `.copy()` allocation round-trip per permutation |
| binary-trees | 2.11x, 2094 MB peak | double payload boxing, never freed |
| k-nucleotide | 2.9x | a heap `String` key manufactured per window position |

The programs that reach parity are the ones whose inner loops hold only
numbers. Every program that carries a value into its inner loop loses, and loses
by the size of the copy.

## What a design has to satisfy

These are constraints, not preferences.

1. **Three-way parity.** The interpreter, the native backend and wasm must
   produce identical bytes. A view that exists in one and not another is not a
   candidate.
2. **No backend implementation.** The fix is expressed in the language, not
   hard-implemented in a backend.
3. **The move checker must still be able to decide.** A view outliving what it
   views is the failure mode this language exists to prevent. Any design that
   cannot be checked statically is refused, however fast it is.
4. **No new cost at a call site that does not use it.** The parity programs are
   at parity. They must stay there.

## The candidate designs, with what each would cost

Marked `RECOMMENDATION, NOT A DECISION`. Each row is a direction, not a
specification.

| design | what it adds | move checker | interpreter | native | wasm | what it cannot do |
| --- | --- | --- | --- | --- | --- | --- |
| **A view type** — `View<T>` naming a range of someone else's array, produced by `read` accessors | one type, one lifetime relation | must learn that a view borrows its source for the view's extent | a range beside the `Rc`, no allocation | a pointer and a length | same as native | cannot be stored in a record that outlives the source, which is most of what the ten modules want |
| **Field borrow** — reading a field of a `read` record yields a borrow rather than a copy | no new type; changes what field access means | must track per-field borrows, which it does not do today | no copy on field read | no copy | no copy | changes the meaning of existing code, so every current reader must be re-checked |
| **A slice that shares the `Rc`** — a substring or subarray that holds a refcount and a range | one type, no lifetime relation at all | unchanged: it owns a refcount, so nothing dangles | cheap, and the representation already refcounts | needs the same header natively | same | keeps the source alive as long as any slice lives, which trades copying for retention |
| **Accept the copy, remove the need** — give the ten modules keyed or computed-once forms so they stop reading in loops | nothing | unchanged | unchanged | unchanged | unchanged | does not close the gap; it routes around it ten times, and the eleventh caller pays again |

The fourth row is included because it is the only one that needs no language
change, and because two of the census's other repeating patterns — constant work
rebuilt per call, in seven modules, and linear-scan lookup, in eleven — would be
fixed by that same work regardless of what happens here.

### What the correction does to this table

**Row B is out.** "Reading a field of a `read` record yields a borrow rather
than a copy" describes a change to something that already does not copy. There
is nothing there to fix.

**Row A is out, in its non-storable form.** A view that cannot outlive the
expression it appears in buys nothing over reading through the field, which is
already 0.86 µs. Its whole value would have been avoiding a copy that is not
happening.

**Row C is the only candidate left**, and the measurement is why: it is the only
one of the four that can be RETURNED and STORED, which is the operation that
costs 490 µs today. Its stated drawback — the source stays alive as long as any
slice does — is now the entire design question, rather than one trade among
four.

**Row D stays available**, and RFC-0113 is evidence it is worth more than this
table gave it credit for: giving `bytes` a range took `std/strpred`'s `slice`
from 57.5 per cent of the site build to 9.2, with no view type and no lifetime
reasoning. That is one of the ten modules routed around, and it cost one arity
on an existing builtin.

### What row C would cost, stated plainly

The interpreter is nearly free: `Val::Str` is `Rc<String>` and `Val::Array` is
`Rc<Vec<Val>>`, so a slice is a refcount and two integers.

Native and wasm are not. An array is `{ptr, len, cap}` and a string is a
`{len, cap}` header with no refcount, so either every array and string grows one
— which is a cost at call sites that never take a slice, and constraint 4
refuses that — or slices become a SECOND type with its own representation, and
every API that takes a `String` needs to say whether it takes a slice too.

That second-type question is the real work, and it is a language design decision
rather than an implementation one. It is still the owner's.


## The question this RFC did not answer, answered

Whether a view can be stored. The ten modules mostly want to *keep* a view — a
parser holding a window into its source, a schema holding a name out of a
document. A view that cannot be stored solves the hot loop and not the library.
A view that can be stored needs the move checker to reason about lifetimes it
does not reason about today.

## The decision (2026-08-29): the trilemma, and the branch this language took twice already

Let `V` be a storable value granting O(1), copy-free access into a buffer
`B`. Soundness means `B` is live at every USE of `V`. There are exactly
three ways to have that, and the list is exhaustive because a stored `V`
either carries proof, carries ownership, or carries neither — and then the
use must check:

1. **Carry proof** — lifetime or region variables in the type system.
   Sound, zero runtime, and the annotations infect every signature a view
   passes through. Rice's theorem closes the annotation-free version:
   whether an arbitrary stored value outlives an arbitrary source is a
   semantic property no analysis decides in general, which is the same
   wall RFC-0120's partition argument hit from the other side.
2. **Carry ownership** — row C's refcounted slice, or a view that consumes
   its source. Runtime cost or exclusivity, plus the second-representation
   question this document already priced.
3. **Check at the use** — `V` is a LOCATOR (an index with extent), and the
   read validates it. Dangling degrades from undefined behaviour to a
   CHECKED outcome: a canonical trap or an honest `None`.

This language chose branch 3 twice before this document existed: every
`a[i]` is bounds-checked with a canonical trap, and `std/slots`' `Handle`
is a storable generational locator whose read answers staleness with a trap
or an `Option` (RFC-0122 priced the checked read at ~3 ns). The decision is
to name that choice rather than add a fourth mechanism: **a stored view is
`(source, locator)` — a byte range into a `String`/`Array`, a path into a
tree, a handle into a container — and the READ is a projection.**

Why this is "proven" in the strict sense: a locator is plain data and
creates NO edge into the heap, so the Heap Forest Theorem
(rfcs/proofs/release-algorithm.md, Part V) — the invariant every release
theorem stands on — is untouched. The design's proof obligation is empty.
The staleness residue is the bounds/generation check's job, already
canonical, already paid. Constraint 4 holds exactly: a program that never
slices sees no representation change and no new checks.

**The re-measurement that sized the remaining work (2026-08-29, at the
RFC-0123 head).** The projection arc landed after this document's numbers:
`j.field(k)` / `j[i]` read at their access sites (RFC-0120..0122), and
chains dispatch (RFC-0123). The census's headline copy — a 4096-element
subtree escaping `fieldAt` by value — re-measures at 168.7 µs; the same
subtree read through the chain is **3 ns**, and walking all 4,096 elements
through chains totals 4.6 µs — a thirty-seventh of ONE escape-copy. The
hot-loop half of this RFC's gap is closed by machinery that shipped for
other reasons, and the stored half already has its locator forms:
`bytes`' range (RFC-0113: 57.5 → 9.2 per cent of the site build),
`indexPath` (~106 ns), `Handle`. **So nothing ships today.** A dedicated
`Range` type with range projections is the recorded next step, and it
lands when a payer turns up holding `(source, start, len)` triples often
enough to hurt — the RFC-0123 M4 bar, applied here.

**The reopen condition, stated so it can fire:** a payer that needs a
SELF-CONTAINED stored view — one that travels without its source. The
trilemma is the proof that no free version of that exists; whoever reopens
this chooses branch 1 or branch 2 with the price list above open.

## Sources

- The five measured shapes: RFC-0108 section 5c.
- The ten modules: `rfcs/census/std-quality/README.md`, pattern 2.
- The benchmark attributions: `rfcs/census/benchmark-gaps.md`.
