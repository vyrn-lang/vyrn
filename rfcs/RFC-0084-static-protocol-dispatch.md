# RFC-0084 — Protocol Dispatch Is Static Everywhere

- **Status:** **Draft.** Designed, not started.
- **Depends on:** RFC-0002 §5 (protocols), RFC-0080 (generic impls, associated
  types — M1 established that selection is a *wider key*, not a new mechanism)
- **Motivated by:** RFC-0074 M1 and M2, which each paid a spelling cost for the
  same missing capability.

## The restriction, and what it actually is

```vyrn
type Box = { n: Int64 }
protocol Bump { fn bump(self) -> Box }
impl Bump for Box { ... }
```

> `impl Bump for Box` is not supported — implement protocols for
> Int64/Bool/String or an enum (validated scalars and records erase at runtime)

So a protocol may target a scalar, an enum, `Option` or `Result`, and **not a
record and not a validated scalar**. The stated reason is exact and still true:
`interp.rs` resolves a protocol method by `val_type_key(&vals[0])` — the
**runtime** value — and a `Val::Record` carries no name, so the interpreter
could not pick an impl where native and wasm can.

## Why it is worth removing

Both compiled backends are already static. The **checker** computes
`type_key(recv)` from the *static* receiver type and resolves straight to
`impl_method_name(proto, key, name)`. Only the interpreter looks at a value.

That asymmetry is the whole restriction. It is not a property of protocols, of
records, or of the runtime representation — it is one engine doing at run time
what the other two do at compile time.

**The cost is already being paid twice, in one RFC.** RFC-0074 designs a fluent
projection API and cannot have it:

```vyrn
get("/{id}", byId).cacheFor(3600).etag().notFoundWhen(|e| ...)   // designed
notFoundWhen(lastModified(etag(cacheFor(GET(byId("/{id}")), 3600)), "created"), ...)  // shipped
```

A method call on a `Route` — a record — has nowhere to resolve, so M2 shipped
outside-in nesting. M1 paid a different instance of the same bill. Every future
library that wants a builder pays it again.

## The design

**Resolve every protocol method call at check time, and let the interpreter call
an ordinary function.** The checker already computes the mangled name; the
change is to make that resolution reach the interpreter instead of being
recomputed from a value.

Then `ok_target` widens to any type `type_key` can name — records and validated
scalars included — because nothing needs a runtime tag any more.

Three consequences worth stating up front:

- **The interpreter's `protocol_methods` map and its `val_type_key` dispatch
  become dead**, for every type and not only for records. Deleting them is part
  of the milestone, not a follow-up: leaving a second resolution path is how this
  project has repeatedly grown a divergence (`charCount` three times, the
  interpreter append three times, `?`-in-a-region two lowerings).
- **A generic call site is the hard case.** Inside `fn f<T: Bump>(x: T)` the
  receiver's type is a parameter, so there is no key until monomorphization.
  RFC-0080 M1 established that dispatch happens through the ordinary
  generic-call path once the impl method is flattened into a generic function —
  so this may already be handled, and that is the first thing to check rather
  than assume.
- **Validated scalars come along for free**, and that is a real gain rather than
  a side effect: `impl Show for Age` is refused today for the same reason, so a
  refinement type cannot carry behaviour.

## The alternative, and why it is the wrong direction

Give `Val::Record` a type name at run time. It is a smaller diff — records are
built through one `construct` path that already has the declaration in hand —
and it costs a word per record.

It is wrong because it makes the interpreter **more** dynamic where the other
two engines are static, to serve a dispatch that is statically known at every
call site. The parity invariant is that three engines agree; the cheapest way to
agree is for all three to do the same thing, and two of them already do.

## Milestones

### M1 — the resolution reaches the interpreter

Whatever mechanism carries it — a rewritten call name in the AST, a side table
the interpreter reads, or resolution at flatten time in the parser, which is
where RFC-0080 M2 put associated types — pick one and say why. Delete the
runtime dispatch in the same milestone.

Pin: every existing protocol example passes **unchanged**, three engines
byte-identical. That is the whole test, exactly as RFC-0080 M3's was.

### M2 — `ok_target` widens

Records and validated scalars become legal impl targets. Pin: `impl Bump for
Box` runs on three engines, and `impl Show for Age` on a refinement type does
too.

### M3 — the fluent projection

`std/http`'s combinators become methods, and RFC-0074's designed spelling
becomes the real one. This is the milestone that pays for the other two, and it
should be measured against the RFC's own example rather than a synthetic one.

## What this does not decide

**Protocols on `Array<T>` or `Map<K, V>`.** `type_key` names them, but their
runtime shape is shared with every other array, and whether an impl on a
container is wanted at all is a separate question.

**Dynamic dispatch.** Nothing here adds a vtable. Every call resolves statically
or is an error; the point is to make the interpreter agree with that, not to
introduce the opposite.
