# RFC-0030 — `if` as an Expression

- **Status:** Draft (design locked)
- **Depends on:** RFC-0022 (`else if` — the statement-side chain this
  mirrors), the match-expression machinery (arm unification, backend
  lowering — everything here rides it)
- **Evidence:** `match` is already an expression, so the language draws an
  arbitrary line straight through its own conditionals; the `.vyx` emitter
  was shaped end-to-end by statement-`if` (RFC-0026 M4 as-landed calls it
  "the single load-bearing constraint" that forced per-node mutable
  accumulators); view code keeps writing
  `let mut x = a  if c { x = b }` for what is one expression.

---

## Surface

```vyrn
let label = if count == 1 { tOneBook() } else { tManyBooks(count) }

fn badge(active: Bool) -> Html {
    return el("span", [cls(if active { "tag on" } else { "tag" })], [
        text(if active { "●" } else { "○" }),
    ])
}

let tier = if score >= 90 { "gold" } else if score >= 50 { "silver" } else { "bronze" }
```

## Semantics (locked)

- **Expression position only, and the statement form is untouched.** An
  `if` at statement start is today's statement (else optional, statements
  inside, no value) — byte-identical behavior, zero corpus churn. An `if`
  in an expression position is the new form.
- **Branches are single expressions in braces:** `if cond { expr } else
  { expr }`, chaining with `else if cond { expr }`. Braces are required
  (no ternary, no brace-less form). **v1 deliberately refuses statements
  inside expression-`if` branches** — no block-value/tail-expression
  semantics is being invented; if a branch needs statements, use the
  statement form or a function. (This mirrors how match arms are
  expressions, and keeps the two consistent.)
- **`else` is mandatory** in expression position (totality) — a missing
  `else` is a checker error naming the rule ("`if` used as an expression
  needs an `else`").
- **Typing:** all branches unify exactly as match arms do (same code
  path); the condition is `Bool` as today. Validated-type coercion rules
  apply to the unified result at the use boundary, unchanged.
- **Evaluation:** condition first, then ONLY the taken branch (laziness
  identical to statement-`if` and match). Ownership/movecheck treats
  branches exactly as match arms (a consume in one branch = the match
  rules, already defined).
- **Everywhere an expression goes:** `let` inits, call args, returns,
  array/map literals, interpolation holes (`"\{if c { a } else { b }}"`),
  match arms, other `if` expressions (nesting composes; the RFC-0023
  no-nested-lambda rule is about lambdas, not this).

## Mechanism

Lower expression-`if` to the existing match lowering (semantically a
two-arm boolean match): the checker reuses arm unification, the
interpreter evaluates the taken branch, codegen reuses the existing
branch+result machinery — no new IR concepts in any backend, parity
inherited. `fmt` keeps short expression-`if` on one line
(source-tightness rule) and formats chains like statement chains.

## Consumers to migrate as evidence (minimal, not a churn pass)

The `std/vyx` emitter sites where the accumulator pattern exists ONLY
because `if` couldn't yield a value (the M4 notes' complaint), plus a
handful of `let mut x = …  if c { x = … }` sites in std/examples where
the expression form is plainly clearer. Corpus-wide rewriting is
explicitly out of scope.

## Out of scope

Ternary syntax, brace-less branches, block-value/tail-expression
semantics (statements in expression branches), `match`-syntax changes,
making the statement form require `else`.
