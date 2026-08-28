# Every place the interpreter opens a scope frame, and whether a compiler could predict it

Read out of `compiler/vyrn-frontend/src/interp.rs` at `combined`. This is the
local half of the slot-resolution question: a static `(depth, index)` is only
safe if the static picture and the interpreter's actual `scope.len()` agree at
every read, and a disagreement is a silent wrong-variable read rather than a
crash.

Seven sites push a frame. Six are predictable. One is not, and it is not close.

| site | when it pushes | predictable? |
| --- | --- | --- |
| `interp.rs:2942` — a lambda call | always: a fresh `vec![captures]`, then a push for the parameters, so a lambda body always starts at depth 2 | **yes** |
| `interp.rs:3096` — a block | only when the block holds a `Stmt::Let`. That predicate is a pure function of the AST node, so a checker computes the same answer | **yes** |
| `interp.rs:4140` — an `if let` binding | only when the pattern matches — but the body runs only then too, so from inside the body the depth is fixed | **yes** |
| `interp.rs:4236` — `for in` | always, once per iteration, with the body's own frame nested inside | **yes** |
| `interp.rs:6279` — a `match` arm | only when the arm is taken, and the arm body runs only then | **yes** |
| `interp.rs:7339` — a stream `for` | always, once per item | **yes** |
| `interp.rs:4399` — an inlined projection body | always, around an AST **built at run time** | **no** |

## The one that is not predictable

`crate::project::inline` (`project.rs:372`) clones a `place` function's body and
then **renames its bindings with a counter that increments per inline**
(`project.rs:381-397`, `collect_bindings` at `project.rs:714`). So the
`Expr::Var` nodes executing there did not exist when the checker ran, and their
names did not either.

The comment at that rename is worth quoting, because it says the interpreter and
the compiled backends already disagree about scope shape here on purpose:

> They land in the CALLER's block: the prologue is statements, not a scope of its
> own. … the store read the wrong element, and only in the two compiling
> backends, because the interpreter gives each inline a frame.

So this is not an oversight to tidy up. The interpreter opens a frame the
backends do not, deliberately, and the names inside it are minted while the
program runs.

## What that means for a design

A scheme that stamps every `Expr::Var` with a `(depth, index)` has to answer for
nodes the checker never saw. The obvious answer is that the stamp is optional —
`Option<(u16, u16)>`, `None` meaning "scan by name" — so resolved nodes are fast
and synthesized ones keep working. That also covers anything else that
synthesizes AST later.

Two facts that make the rest easier than feared:

- **A lambda body is cloned, not re-parsed** (`interp.rs:2866`, `body:
  body.clone()`). A stamp stored in the node survives the clone, so captures need
  no special handling on that account.
- **Vyrn's captures are by copy already**, snapshotted where the lambda is
  written. There is no cell, no upvalue, no shared mutable environment to
  represent.

## What the prize is now

The scan a stamp would remove is no longer a hash. After the work in
`rfcs/census/interpreter-loop-cost.md`, a read walks 1.30 frames and 99.2% of
frames hold three bindings or fewer, so the cost is about four `str` comparisons,
each of which rejects on length before it compares a byte.

That is the number any design has to beat, and it is much smaller than the number
that made slot resolution look obvious.
