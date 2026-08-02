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
record and not a validated scalar**. The stated reason is exact: `interp.rs`
resolves a protocol method by `val_type_key(&vals[0])` — the **runtime** value —
and a `Val::Record` carries no name, so the interpreter could not pick an impl
where native and wasm can.

The one word doing the work there is *carries*. It is a fact about the value
representation, not about protocols, and the two refused cases turn out to
differ on exactly that word — see the design below.

## Why it is worth removing

Both compiled backends are already static. The **checker** computes
`type_key(recv)` from the *static* receiver type and resolves straight to
`impl_method_name(proto, key, name)`. Only the interpreter looks at a value.

That asymmetry is the restriction. It is not a property of protocols — it is one
engine doing at run time what the other two do at compile time, and being able
to answer for fewer types as a result.

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

The draft of this RFC proposed rebuilding dispatch statically everywhere:
resolve every protocol call at check time, carry the mangled name to the
interpreter, and delete `val_type_key`. Reading the code changed the answer, and
the reason is worth keeping rather than the conclusion.

**The two paths do not disagree. They agree on every type they both admit, and
the admitted set is drawn exactly where they would stop agreeing.** For a scalar,
an enum, `Option` or `Result`, `type_key(static type)` and
`val_type_key(runtime value)` return the same string by construction. The
refusal list is not "types the interpreter cannot see"; it is "types where the
runtime value has forgotten which static type produced it" — a validated scalar
(`Age` is a `Val::Int`) and a record (`Val::Record` is a bare `HashMap`).

Those two cases are not the same case.

**A record has room for its name.** And the place to put it is not construction —
it is `coerce`, the interpreter's typed boundary, which already rebuilds the map
at every let, param, return, field and element. A record literal's type comes
from its context, so stamping the name in `coerce` means the interpreter's
dispatch key is **derived from the static type**, the same source the compiled
backends read. It is not a second, more dynamic mechanism; it is the static
answer, cached in the value at the boundary where the static answer is known.

**A validated scalar has no room.** `Val::Int` cannot carry `Age` without a
wrapper, and a wrapper puts a branch in every arithmetic path in the
interpreter — which is where numeric parity is thinnest. That case genuinely
needs the resolution to travel from the checker, and travelling requires
**call-site identity**: the existing side tables key on `(line, name)`, which is
enough for a `let` and not enough here, since `a.show() + b.show()` with two
receiver types on one line is a legal program that (line, name) cannot tell
apart. So the honest cost of validated scalars is an id on `Expr::Call`, a
resolution map from the checker, and a rewrite pass — recorded here so it is not
re-derived, and not built until something needs a refinement type to carry
behaviour.

Splitting there costs nothing that is currently wanted: **M3, the milestone that
pays, needs records.** `Route` is a record.

### What stays true

- **`val_type_key` stays**, and stays honest: it now answers for every type it
  admits, and returns `None` for exactly the types `ok_target` refuses. The two
  functions are a matched pair and should be read as one.
- **A generic call site is unaffected.** Inside `fn f<T: Bump>(x: T)` the
  receiver is a type parameter and dispatch already goes through the ordinary
  generic-call path (RFC-0080 M1) once the impl method is flattened into a
  generic function by the parser.

## Milestones

### M1 — a record knows its type at run time

`Val::Record` carries its declared name, stamped in `coerce`; `val_type_key`
answers from it; `ok_target` admits a record target. Nothing in the compiled
backends changes — they were already static.

Pin: `impl Bump for Box` runs on three engines, byte-identical, and every
existing protocol example passes unchanged.

The name must come from the boundary and not from `construct`: a record built
from a literal and then coerced into a differently-named type of the same shape
must dispatch as the type it was coerced to, because that is what the checker
told the compiled backends.

### M2 — the fluent projection

`std/http`'s combinators become methods, and RFC-0074's designed spelling
becomes the real one. This is the milestone that pays for M1, and it should be
measured against RFC-0074's own example rather than a synthetic one.

### Not a milestone — validated scalars

`impl Show for Age` stays refused, with the mechanism above written down. The
diagnostic should say which of the two reasons applies rather than naming both,
since after M1 they are different reasons.

## What this does not decide

**Protocols on `Array<T>` or `Map<K, V>`.** `type_key` names them, but their
runtime shape is shared with every other array, and whether an impl on a
container is wanted at all is a separate question.

**Dynamic dispatch.** Nothing here adds a vtable. Every call resolves statically
or is an error; the point is to make the interpreter agree with that, not to
introduce the opposite.
