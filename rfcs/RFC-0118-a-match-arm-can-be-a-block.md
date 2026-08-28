# RFC-0118 — a match arm can be a block

- **Status:** Proposed, direction decided (2026-08-28, on the user's "finish
  the design decisions"): block-bodied arms in STATEMENT position, not `drop`
  as an expression. Implementation is the open half.
- **Evidence:** the per-node census
  (rfcs/census/binary-trees-per-node.md): binary-trees' release walk makes
  **two calls per node where one would do**, and the second call exists only
  because a `match` arm is a single expression and `drop` is a statement. The
  trampoline it forces (`give(l)`; `give(r)`) spills, calls, and frees per
  child — the census names removing it as the gap's "most concrete, bounded
  piece."

## The gap, and how often the language pays it

A `match` arm is one expression. A body that needs two statements — release a
payload, then another; bind, then print; anything sequential — must become a
function, called from the arm. The tree carries this workaround under three
names already, and grew a fourth the day this RFC was decided:

- `give` in `examples/binarytrees.vyrn` — the measured one: each call spills
  its argument, calls `release`, frees the boxes. Two calls per node.
- `jsonGive` in `std/json` — "Hands one payload back to its own release row,
  so an arm can be an EXPRESSION," its own comment says.
- `vyxGive` in `std/vyx` — the same, its named twin.
- `report` in `examples/wirekey.vyrn` — not even a release: a decoded value
  that wanted three prints and a loop, exiled to a function "because a
  `match` arm is a single expression."

Two of the four exist to spell `drop` in an arm; two exist for ordinary
sequencing. That split is the decision's whole argument.

## The decision: blocks, not `drop`-as-expression

The census offered two fixes. `drop` as an expression serves exactly the
release case: it would still leave `report` exiled, it needs an answer for
what type a `drop` expression has (and then every release impl keeps its
`=> 0` arms so the arms agree), and it makes a statement into an expression
in one special position — a grammar wart with one customer. Block arms serve
all four names, align `match` with `if` (whose branches have been blocks
since the beginning), and erase the `=> 0` contortion too: in a statement
match, arms yield nothing, so nothing has to agree.

**The containment is the design.** Only a `match` in STATEMENT position may
have block arms:

```vyrn
match consume self {
    Leaf => {}
    Node(l, r) => {
        drop l
        drop r
    }
}
```

A `match` in EXPRESSION position is unchanged: single-expression arms, one
arm type, exactly as today. This is what keeps the change from touching the
value story at all — no "last expression is the block's value" rule, no arm
type unification against block tails, no formatter question about where a
value-block's result sits. A statement match's arms are checked as blocks and
yield nothing; an expression match never sees a brace. The two forms share
the scrutinee machinery (Rule N's edge releases, the temporary's row, binder
scopes) that RFC-0114 already built per arm.

Mixed arms are legal in a statement match — `Leaf => {}` beside
`Node(l, r) => free(l, r)` — an expression arm's value is discarded, as any
expression statement's is.

## What it buys, priced in advance

The release walk stops paying the trampoline: `release` recurses into itself,
one call per child instead of `give`'s spill-call-free. The census's
prediction is the release walk's call count HALVED on binary-trees; M2 is
where that number is measured rather than promised, interleaved in one
window, before-and-after, the way this repository prices claims. The three
`give` twins and `report` are deleted in the same milestone, which is the
ergonomics half made concrete: four functions that exist only to stand where
a block could not.

## Milestones

- **M1** — the surface: parser (statement-position `match` accepts
  `pat => { stmts }` arms; expression position refuses a brace with a named
  diagnostic), checker (arm blocks walked as blocks, no value), movecheck/own
  (arm blocks enter the same branch scopes Rule N's arm walks already
  maintain), interpreter and both compiled backends (arm blocks lower as
  statement blocks into the existing join), `vyrn fmt`, and the LSP's
  outline. A corpus witness with a multi-statement arm; the refusal —
  a block arm in expression position — pinned in EXPECTED_CHECK_FAILURE.
- **M2** — the payoff: `give`/`jsonGive`/`vyxGive`/`report` deleted, their
  sites rewritten as block arms; binary-trees re-measured interleaved with
  the census's numbers updated; the census page closes its finding.
