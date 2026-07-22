# RFC-0060 — Control-Flow Ergonomics: `break`/`continue`, `if let`/`while let`, `%`

- **Status:** Locked design
- **Depends on:** RFC-0022 (`else if`), RFC-0030 (if-as-expression), the
  leak/race hardening arc (drop discipline on every exit path — the hard
  part of `break`), RFC-0045 (bitwise — the operator-lowering precedent)
- **Evidence (dogfood):** every one of the three dogfood apps (shelf, bin,
  vlog) tripped on the same absences, recorded in their friction reports:
  no early loop exit (contorted `mut done` flags), no `if let` (full
  `match` ceremony for a single Option probe), no `%` (hand-rolled
  `a - (a / b) * b`).

Three features, one RFC, because `while let` desugars onto `break` and all
three are the "daily-driver" batch.

---

## 1. `break` / `continue`

```vyrn
for x in xs {
    if x == needle {
        found = true
        break
    }
}
while true {
    let line = readLine()
    match line {
        Some(s) => process(s),
        None => break,
    }
}
```

- Statements, legal only inside a `for`/`while` body — a checker error
  elsewhere (`` `break` outside a loop ``, same for `continue`), including
  inside a spawned block's non-loop code and at test/bench top level.
  Unlabeled only; loop labels are out of scope.
- `break` exits the INNERMOST loop; `continue` jumps to its next
  iteration (the condition/step re-evaluates exactly as if the body had
  ended normally).
- **Drop correctness is the bar.** Leaving the body early must run
  exactly the drops a normal iteration-end/loop-exit would: locals of the
  body scope (and any nested scopes being exited), owned iteration
  bindings, region interactions. The leak accounting (RUNTIME_FREES
  parity conventions) must balance on `break`/`continue` paths, including
  from inside nested blocks and `if let` bodies. Movecheck treats code
  after `break`/`continue` in the same block as unreachable (the `return`
  precedent), and a value moved on one arm's break-path must not be
  considered moved on the fall-through path.
- All three backends: interp (control signal, the `?`-prop precedent),
  native/wasm (branch to the loop's exit/latch block AFTER emitting the
  scope drops). Byte-identical everywhere, including trap interactions.

## 2. `if let` / `while let`

```vyrn
if let Some(v) = cache.get(key) {
    return v
} else {
    logger(1).info("miss: \{key}")
}

while let Some(line) = readLine() {
    out.push(parse(line))
}
```

- **Pattern grammar = the `match` arm pattern grammar**, restricted to
  refutable enum-variant patterns with binders (`Some(x)`, `Ok(v)`,
  `Err(e)`, user enum variants incl. multi-payload). No literal patterns,
  no nesting beyond what match arms already do — this is match's pattern,
  not a new one.
- `if let P = e { A } else { B }` — `else` optional; `else if` and
  `else if let` chain (RFC-0022 composition). It is a STATEMENT form in
  v1 (not an expression — `let x = if let …` is a checker error with a
  hint to use `match`).
- `while let P = e { body }` — re-evaluates `e` each iteration, runs
  `body` with the binders while the pattern matches. `break`/`continue`
  work inside (it IS a loop).
- **Pure frontend desugar** to existing `match`/loop AST — zero backend
  work, and movecheck/ownership/drop analysis see the desugared form so
  every existing rule (payload moves, drop obligations) applies verbatim.
  The desugar must not double-evaluate `e` per iteration and must
  attribute diagnostics to the source `if let` line/col (not synthetic
  positions).
- fmt formats both forms stably; LSP: binders are real locals (hover,
  go-to-def, document highlight, completion in scope), the editor
  grammar needs no change beyond what `match` patterns already get.

## 3. `%` — integer remainder

- Integers only, all sized signed/unsigned types, same type rules as `/`
  (both operands the same integer type). On floats: checker error with a
  hint (`no `%` on Float64; integer remainder only`).
- Precedence and associativity identical to `*`/`/`.
- Semantics: truncated remainder, sign of the dividend (the C/Rust/LLVM
  `srem` convention). `a % 0` traps with canonical wording exactly
  parallel to division's zero trap (reuse the established phrasing
  family). `INT_MIN % -1 == 0` — NO trap, consistent with the wrapping
  overflow philosophy and with the identity below; the native/wasm
  lowering must guard this case explicitly (raw LLVM `srem` is UB there).
- Law, test-pinned across all int types and backends:
  `a == (a / b) * b + a % b` for every non-zero `b` (under wrapping).
- Usable in consteval/refinement predicates wherever `/` is.

## Verification

1. Drop/leak matrix for `break`/`continue`: owned locals in the body,
   owned iteration values, nested blocks, break from inside `if let`,
   `continue` under a region, spawn-body loop — RUNTIME_FREES balances,
   three-way byte-identical.
2. Movecheck: use-after-move across break paths rejected; code after
   `break` unreachable-clean.
3. Checker errors pinned: `break`/`continue` outside loops, `if let` as
   expression, `%` on floats.
4. `while let` over a draining source terminates; desugar does not
   double-evaluate (pin with a side-effecting scrutinee).
5. `%`: the law above property-style over a value table incl. INT_MIN,
   -1, mixed signs; zero-divisor trap wording byte-identical to `/`'s
   family across backends.
6. A parity example exercising all three features together
   (`examples/controlflow.vyrn`); fmt idempotent on it.
7. Full suite + LSP + parity green; 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy (parser/checker changed).

## Out of scope

Loop labels, `break value` (loop-as-expression), `let … else`, literal
and nested irrefutable patterns in `if let`, float remainder, and a
`loop { }` keyword (write `while true`).
