# RFC-0121 — a pattern is a place

- **Status:** Implemented (2026-08-29), the same day RFC-0120 landed. The
  refutable `let` parses and desugars onto `match` + `Pattern::Other`; the
  three engines print byte-identical traps through the ordinary `panic`
  path; a read projection's place roots transitively through borrowing
  prologue `let`s; `std/json` gained `j[i]` and `j.field(key)`; the witness
  benches price the census's 4096-element lookup at 3 ns in place against
  43 ns copied (and the copy scales with the subtree — the census's shape
  paid 201.6 µs; the place is flat). Four latent gaps surfaced and closed
  on the way — see "What the implementation found".
- **Depends on:** RFC-0120 (result capabilities — the projections this feeds),
  RFC-0091 (the expansion model), RFC-0060 (`if let`), RFC-0030/0118 (`match`
  as an expression, whose lowering carries the whole feature)
- **Evidence:** the std-quality census's largest live row. `jsondec`'s
  accessors deep-copy a subtree per level — 201.6 µs for one lookup in a
  4096-element array against 0.18 µs for the scalar equivalent — and
  RFC-0120's adoption pass recorded exactly why they could not convert: the
  `Json` value is an enum, and a projection's place could not reach through
  a payload.

## The gap, stated in the machinery's own terms

A projection inlines as a flat expansion: a prologue of statements, then one
place expression (RFC-0091 M2, unchanged by RFC-0120). `Json` is an enum, so
any place inside it starts with a payload access — and the only payload
accessor in the language is a pattern, whose binders live inside a `match`
or `if let` arm. An arm is a scope the engines pop; the expansion's place is
read after the prologue, outside any arm. Multi-exit projections were
refused by RFC-0091 rule 1 on purpose — a branch over two places is a shape
no engine lowers — and this RFC keeps that refusal. The fix is not control
flow in projections. It is a payload binding that lands in the **enclosing**
scope, so the body stays straight-line and the place stays last.

## The design

### M1 — a refutable `let` binds or traps

```vyrn
let JArr(items) = self
```

A `let` whose left side is a variant pattern binds the payload in the
enclosing scope, or traps. This is the philosophy the language already has —
`a[i]` traps, `s[h]` traps — and the tolerant path keeps its spelling:
`if let` is unchanged, and a body that wants its own wording checks first
and panics in its own words.

**The whole mechanism is a parser desugar onto `match`:**

```vyrn
let items = match self {
    JArr(items) => items,
    <other> => panic("let `JArr(..)` did not match"),
}
```

where `<other>` is `Pattern::Other` — a default arm that matches any
remaining variant, unspellable in source, produced only by this desugar.
The precedent is `Pattern::Success`/`Failure`, which the `??` desugar mints
for exactly the same reason: the parser has no type information, so the
pattern that "means the rest" has to be resolved by the checker and the
engines, not spelled by the user. `Pattern::Other` is that arm in its
simplest form: it tests true, binds nothing, and makes the `match`
exhaustive by construction.

Everything downstream is existing, tested machinery: `match` as an
expression lowers in all three engines; payload binders are borrows
("payload borrows, never drop-tracked" — the native backend's own words);
the trap is an ordinary `panic`, so the message carries the loader-stamped
site and is byte-identical across engines for free.

Rules:
- The scrutinee must be a NAME — `self`, a parameter, a local. A
  multi-payload pattern desugars to one `match` per binder over that name,
  and a name is what makes the repeated read the same read. Anything else
  is refused: "bind it to a name first".
- Enum variants only. `Some`/`Ok`/`Err` keep `if let` and `?` — nothing
  wants a trapping destructure of an `Option` — and a record needs no
  pattern, it has fields.

### M2 — a read projection's place may root in a borrowing `let`

`place_root` today demands the chain end at `self` or a parameter, so
`return items[i]` is refused as somebody else's place even when `items` is
a borrow of `self`'s own payload. For READ projections the rule becomes
transitive: a root that is a prologue `let` may root where its initializer
roots, when the initializer is itself a borrow — a field or element chain
of an already-accepted root, or the M1 `match` shape, where every
non-diverging arm yields a binder of its own pattern and the root passes
through the scrutinee. The trace must still bottom out in the receiver; the
borrow discipline is unchanged, and movecheck already calls each link a
borrow (`payload_binding`, `element_path`).

MODIFY projections keep the strict direct-rooted rule. A read through a
copied handle reads the same bytes; a write through one can land in the
copy — `atSet`'s place stays a chain the store machinery can prove writes
through.

The expansion model is untouched: the prologue's `let`s bind at the access
site, the place reads through them in the same scope, and all three engines
already execute exactly that.

### M3 — the adoption: `jsondec` reads in place

`Json` gains an `Index` impl: `j[i]` is the element place of a `JArr`
(trapping on kind and bounds, as `a[i]` does), and a named `field(key)`
projection is the value place of an object member (trapping on kind and
absence). The tolerant, copying `elemAt`/`fieldAt` stay for the
JNull-on-miss paths and for every existing caller — the projections are the
fast spelling beside them, and a bench prices the census row through them.

## What the implementation found

Four latent gaps, none of them this RFC's design — each a walk that never
had to answer for a shape until this RFC made the shape writable:

- **The loader never renamed `if let` variant patterns.** `match` arms got
  the imported-enum rename; `if let JStr(s) = ..` over an imported variant
  had never been written in the corpus, and failed with "`JStr` is not a
  variant of enum { json$JStr | .. }". Fixed beside the `match` arm rename.
- **`project::inline`'s hygiene missed pattern binders and assignment
  targets.** The rename walk covered `let`s and lambda parameters; a match
  arm's binder kept its name while `subst_block` rewrote its uses ("unbound
  variable `@b0.fields`"), and `hit = k` was the first projection body that
  ever assigned a local. Both walks extended.
- **movecheck read a payload binding as a fresh owner.** `let items = match
  g { JArr(items) => items, .. }` on module state minted an owned
  reclamation row; `own` freed the global's buffer at block exit, and the
  loop crashed at iteration two — `vyrn why --memory` said "reclaimed at
  block exit — freeing the array buffer" verbatim. `names_a_place` now
  answers "the payload of a place somebody owns" when the scrutinee itself
  names a place; a matched TEMPORARY keeps handing its payload over, for
  the same reason a field of a temporary does.
- **A projection on a projection's result type-checked and then trapped in
  three wordings.** No engine's receiver probe resolves a call result, so
  the checker now refuses the chain — "bind the inner access with a `let`,
  then read through the binding" — and the refusal is the recorded gap:
  chaining works the day the probes learn call results, and nothing
  written today changes meaning when they do.

## What this is not

- Not multi-exit projections: rule 1 stands; every projection still has one
  exit, and the trap is how the other paths end.
- Not a new statement, primitive, or trap: the engines see a `match`
  expression they have lowered since RFC-0030, with one new pattern arm
  whose test is `true`.
- Not views: nothing here lets a place outlive its access site.

## Milestones

- **M1** `Pattern::Other` through the checker and the three engines; the
  refutable `let` parses and desugars; refusals for a non-name scrutinee
  and a non-variant pattern, each with a witness.
- **M2** `place_root` traces through borrowing prologue `let`s for read
  projections; `check_places`' message names the rule.
- **M3** `std/json` grows the `Index` impl and `field`; the witness example
  prices the census row through all three engines.
