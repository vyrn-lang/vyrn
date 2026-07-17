# RFC-0030 — `if` as an Expression

- **Status:** Implemented (M1)
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

---

## As landed

Shipped exactly as designed; the statement form is byte-identical (the
existing corpus is the regression suite) and no backend grew a new IR
concept. **The whole feature rides the `match`-expression machinery.**

- **One new AST node.** `Expr::IfExpr { cond, then_branch, else_branch:
  Option<Box<Expr>>, line }` (`ast.rs`). An `else if` chain nests as the
  `else_branch` `IfExpr` (self-similar, like the `else if` statement chain
  of RFC-0022). `else_branch` is `Option` so a missing `else` parses
  cleanly and the *checker* — not the parser — reports the totality rule;
  every backend may then assume `Some`.
- **Parser.** `stmt()` still dispatches a leading `if` to `if_stmt`
  (statement form, untouched); an `if` reached in *expression* position
  (`primary()`) calls the new `if_expr`. Each branch is parsed by
  `if_branch`: eat `{`, parse exactly ONE expression, require `}`. A
  leading statement keyword (`let`/`return`/`while`/…) or leftover tokens
  before the `}` yield the explicit "single expression in each branch, not
  statements" diagnostic. Inside the braces `no_struct` is cleared, so a
  bare `Name { … }` is a struct literal again. A nested `if`/`match` is an
  expression, so it composes.
- **Checker.** `check_if_expr` is `check_match`'s twin: mandatory-`else`
  totality error ("`if` used as an expression needs an `else`"), `Bool`
  condition, then the two branches folded through the SAME `unify_arm`
  helper the match arms use — so widening (a raw-`Int` branch meeting an
  `Age` branch → the wider type) and use-boundary validated-type coercion
  are inherited verbatim.
- **Backends.** Interp evaluates the condition then only the taken branch
  (`interp.rs`). Textual-IR codegen adds `gen_if_expr`: a `br` to two
  branch blocks and a `phi` at the join — a copy of `gen_match`'s
  Option/Result merge (`void`-typed → no phi, same as match). Movecheck
  treats the two branches as match arms (may-consume merge). The excluded
  Inkwell backend returns an "unsupported" stub, like its other gaps.
- **The vyx-emitter simplification (the evidence).** The M4 accumulator
  complaint was the `let mut head = "if "  if bi > 0 { head = "} else if "
  }` shape in `std/vyx.vyrn`'s `vyxEmitIf`, plus twin `if a.dyn { … } else
  { … }` value-picks in `vyxEmitAttrs`, a `nameStart` offset, and
  `std/rpc.vyrn`'s `lead` separator. Each collapsed to a single
  `let x = if c { … } else { … }`. These recompute the *same* emitted
  strings, so the vyx/rpc goldens are unchanged — the win is in the
  emitter's own source, exactly the constraint M4 flagged. (The
  children-building `mut Array<Html>` accumulators stay imperative — they
  append zero-or-more nodes, which is not what a value-yielding `if`
  replaces.)

    before:  `let mut head = "if "`
             `if bi > 0 { head = "} else if " }`
    after:   `let head = if bi > 0 { "} else if " } else { "if " }`

- **fmt.** No formatter change: the printer is token-based, so an
  expression-`if` formats by the same `if`/`else`/`{`/`}` spacing rules as
  the statement form, never joins/splits lines, and the re-lex safety
  invariant holds (verified on messy input). A short one-liner stays a
  one-liner; a chain formats like a statement chain.
- **LSP.** No new features; the server is a pure adapter over the
  frontend, rebuilt and redeployed (`editor/vscode/server/vyrn-lsp.exe`,
  release) since the frontend changed.
- **Tests / parity.** New `examples/ifexpr.vyrn` is a three-way parity
  citizen exercising every position (let init, arg, return, array element,
  interpolation hole, match arm), the `else if` chain, nesting, the
  laziness proof (only the taken branch's side effect prints), and
  unification into a validated `Age`. 14 new unit tests (parser/checker/
  interp, incl. a trap-in-untaken-branch laziness test and a
  statement-form-still-allows-missing-`else` guard). Full corpus parity
  (interp == native == wasm) stays green.

### Deferred

Statements/tail-expressions inside expression-`if` branches
(block-value semantics) remain deliberately out of scope, as does any
ternary or brace-less form — unchanged from the design above.
