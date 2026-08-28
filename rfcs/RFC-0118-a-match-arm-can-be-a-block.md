# RFC-0118 — a match arm can be a block

- **Status:** M1 and M2 Implemented (2026-08-28, the same day the direction
  was decided on the user's "finish the design decisions"): block-bodied arms
  in STATEMENT position, not `drop` as an expression, in all engines, the
  formatter and the LSP; the four trampolines deleted. **M2's measurement
  came back a wash** — see "What it bought, measured" — so the change stands
  on the ergonomics argument, which was the decision's spine anyway.
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

## What it bought, measured

**The wall clock: a wash.** Binary-trees at depth 21, interleaved old/new,
three rounds each in one window: 18.3/22.2/22.0 s against 18.2/22.1/21.7 —
the differences are smaller than the round-to-round noise, and the outputs
are byte-identical. The census predicted the release walk's call count
halved; that prediction was read off the EMITTED IR, and the optimizer was
evidently already inlining `give` before the machine ever saw a second call.
The lesson joins the noalias wash and the declined pool on the same page:
what the emitted IR spends is not what the machine spends, and a claim about
calls is priced after `-O2`, not before.

**The ergonomics: real, and the decision's actual spine.** Four functions
existed only to stand where a block could not — `give`, `jsonGive`,
`vyxGive`, `report` — and all four are deleted, their sites now saying `drop`
(or their three prints) directly in the arm. The `=> 0` contortion is gone
from every release impl with them.

**And the deletion caught the trampoline doing something it never advertised:
laundering the `drop` rule.** `drop` names a binding whose type is heap or
declares `impl Owned`; a plain record is refused. `vyxGive<T>(v: consume T)`
took the record as a type parameter, and `drop v` on a `T` passes the check
the direct spelling fails — so `VNIf`'s `els` (`{ nodes: Array<VyxNode> }`)
was being dropped through a hole in the rule's coverage. The block arm says
it honestly now: `let elsNodes = consume els.nodes` then `drop elsNodes`, the
scalar shell going with the arm — a spelling that needed a block to exist.

## Milestones — as landed

- **M1** — the surface, all of it: parser (statement-position `match` accepts
  `pat => { stmts }` arms, no comma needed after a block; expression position
  refuses with a named diagnostic through the checker's position flag — the
  arms-slice address set at `Stmt::Expr`, consumed at `check_match`, so a
  nested match never inherits it), checker (arm blocks checked as blocks,
  contributing no type; expression arms beside them still unify among
  themselves), movecheck (arm blocks walk as statements inside the same
  binder scope and branch stamp; a block arm's `arm_heap` is `false` because
  no value exists to alias), own (arm blocks get frames and placement as an
  `if` branch's do), the interpreter (a dedicated statement path, so `break`
  and `continue` inside an arm reach the enclosing loop), both compiled
  backends (any block arm forces the existing void merge, so no phi ever
  meets a valueless edge; the textual backend discards expression-arm values,
  the direct backend `Drop`s them), `vyrn fmt` (the token-stream formatter
  handled arm braces without a change), and the LSP (arm-block lets hover
  like any block's). `examples/blockarms.vyrn` is the corpus witness —
  statements, mixed arms, `break` from an arm, and the trampoline-free
  release — byte-identical three ways under the free audit;
  `examples/blockvalarm.vyrn` pins the refusal.
- **M2** — the trampolines deleted and the claim measured; both results
  above.
