# RFC-0110 — A Lambda Takes Its Parameters Before an Arrow

- **Status:** **Implemented.** The syntax changed, every site in the repository
  moved with it, and the old spelling now produces a diagnostic that names the
  new one. 1,870 tests, three-way parity green at 40.

## What changed

| before | after |
| --- | --- |
| `\|x\| x + 1` | `x -> x + 1` |
| `\|a, b\| a + b` | `(a, b) -> a + b` |
| `\|\| 7` | `() -> 7` |
| `\|x\| { return x * x }` | `x -> { return x * x }` |

The body is unchanged: an expression, or a block. Everything else about a lambda
— monomorphization, capture by copy at the point of writing, defunctionalization
into a closed enum (RFC-0023, RFC-0037) — is untouched. This is spelling.

## Why Java's shape

The owner asked for it: *"I want to replace lambdas syntax from `||` to more
canonical like Java."*

`rfcs/census/lang/lambda-syntax.md` surveyed sixteen languages and set out the
parsing conflict each candidate has in a language with Vyrn's features. It
recorded the `->` conflict this way:

> **Conflict:** `->` already means "return type" in Vyrn. … a `->`-bodied lambda
> needs a marker that says "this is a parameter list, not a type." Without one,
> `|x| -> x` versus `(x) -> x` versus `fn(Int64) -> Int64` collide.

**That is not right, and the reason it is not right is the point of this
section.** The two arrows never share a context. A return type is written where
a TYPE is expected; a lambda is written where an EXPRESSION is expected; and no
position in the grammar takes both. `fn(Int64) -> Int64` is parsed by the type
parser, which never calls the expression parser and never sees a lambda.

So no marker is needed, and the parser needs no backtracking:

- `x -> …` — one token of lookahead. An identifier followed by an arrow, in
  expression position, is a lambda. There is nothing else it could be.
- `(a, b) -> …` — a scan of the parameter list. A list is names and commas and
  nothing else, so the first token that is neither ends the scan and the answer
  is no. The scan is bounded by the list, never by the file.
- `() -> …` — two tokens.

`(x)` on its own is still a parenthesised expression. The arrow after the
closing bracket decides, never the brackets — which is the same rule Java's
grammar states as a separate `LambdaParameters` non-terminal.

Vyrn has no tuple expression, which is what makes the second case cheap: `(a,
b)` cannot be an expression, so a parameter list is the only reading left.

## What the old spelling does now

A `|` is infix-only — it never starts an expression — so a `|` in expression
position can only be a lambda written the old way. The parser says so:

```
`|x| ...` is not a lambda here; a lambda takes its parameters before an arrow
  fix: `x -> ...`, or `(x, y) -> ...` for more than one
```

and for the empty list:

```
`|| ...` is not a lambda here; a lambda takes its parameters before an arrow
  fix: `() -> ...`
```

There is no deprecation period and both spellings never work at once. A file is
one or the other, and the diagnostic converts it.

## What the change cost

Sixty-two sites in eighteen files, plus the Vyrn fixtures embedded in Rust test
sources. The language census had put the figure at 65,842 lines; the real number
was 69 sites in 30 files, and the correction is recorded in that file. A count
three orders of magnitude too large is what a `grep` gets when it sweeps
`.claude/worktrees/`, which holds full copies of the repository.

The formatter got smaller. A `|` had three readings and it tracked two flags to
tell them apart — `in_lambda_params`, and `in_type_decl` because an
enum-variant `|` also follows `=`. Two readings are left, and the second is
decided by the indent rule alone, so the `Tok::Pipe` arm, both flags, the
`lambda_open`/`lambda_close` roles and two `wants_space` parameters are gone.

## What this does not change

- `|` as infix bitwise or (RFC-0045).
- `|` introducing an enum variant on a type's right-hand side.
- `||` as logical or.
- `=>` in a match arm.
- Anything about how a lambda is compiled.

## Verification

- 1,870 workspace tests.
- Three-way parity at 40: every example prints the same bytes under the
  interpreter, the native binary and wasm. That is the check that matters,
  because a mis-migrated lambda would compile and behave differently rather
  than fail.
- `vyrn fmt --check` is clean and idempotent over `std/`, every example, and the
  site.
- The parser's own unit test covers all three shapes, and the block body.
