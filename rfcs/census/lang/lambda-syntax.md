# Census — lambda syntax

Vyrn writes a lambda as `|x| x % 2 == 0`. The owner wants a more canonical shape, like Java's. This file collects what every relevant language does, names the parsing conflicts each shape brings into a language that also has Vyrn's features, counts the work, and lists every place the current syntax is produced or consumed. It ends with options, not a choice.

## The cross-language survey

For each language: no parameters, one parameter, several parameters, a typed parameter, a block body, and a trailing lambda as the last argument.

### Java

| form | example |
|---|---|
| no parameters | `() -> 42` |
| one parameter | `x -> x * 2` |
| several parameters | `(x, y) -> x + y` |
| typed parameter | `(int x) -> x * 2` |
| block body | `(x) -> { return x * 2; }` |
| trailing lambda | none — the last argument stays inside the parentheses |

A single parameter may drop the parentheses. Several parameters, a typed parameter, or no parameters require them. The arrow is `->`. The grammar is JLS §15.27 (https://docs.oracle.com/javase/specs/jls/se22/html/jls-15.html#jls-15.27).

### C\#

| form | example |
|---|---|
| no parameters | `() => 42` |
| one parameter | `x => x * 2` |
| several parameters | `(x, y) => x + y` |
| typed parameter | `(int x) => x * 2` |
| block body | `x => { return x * 2; }` |
| trailing lambda | none — the last argument stays inside the parentheses |

A single parameter may drop the parentheses. The arrow is `=>`. The grammar is the C\# specification §12.19 (https://learn.microsoft.com/en-us/dotnet/csharp/language-reference/proposals/csharp-3.0/expression-trees#lambda-expressions).

### Kotlin

| form | example |
|---|---|
| no parameters | `{ 42 }` or `{ -> 42 }` |
| one parameter | `{ x -> x * 2 }` |
| several parameters | `{ x, y -> x + y }` |
| typed parameter | `{ x: Int -> x * 2 }` |
| block body | `{ x -> val r = x * 2; r }` (last expression is the result) |
| trailing lambda | `foo(a) { x -> x * 2 }` — yes, the last argument may move outside the parentheses |

Parameters sit inside braces, before `->`. A no-parameter lambda may omit the arrow. Kotlin is the one language here with a real trailing-lambda syntax: if the last parameter is a function type, a brace block after the call moves outside the parentheses (https://kotlinlang.org/docs/lambdas.html#passing-trailing-lambdas).

### Scala

| form | example |
|---|---|
| no parameters | `() => 42` |
| one parameter | `x => x * 2` |
| several parameters | `(x, y) => x + y` |
| typed parameter | `(x: Int) => x * 2` |
| block body | `(x) => { val r = x * 2; r }` |
| trailing lambda | `foo(a)(x => x * 2)` or `foo { x => x * 2 }` (a single curried argument) |

The arrow is `=>`. A single parameter may drop the parentheses. Scala 3 also allows `x => x * 2` and the placeholder `_`. A single-argument call may use braces instead of parentheses (https://docs.scala-lang.org/tour/functions.html).

### Swift

| form | example |
|---|---|
| no parameters | `{ 42 }` or `{ () -> Int in 42 }` |
| one parameter | `{ x in x * 2 }` |
| several parameters | `{ x, y in x + y }` |
| typed parameter | `{ (x: Int) in x * 2 }` |
| block body | `{ x in let r = x * 2; return r }` |
| trailing lambda | `foo(a) { x in x * 2 }` — yes, the last closure may move outside the parentheses |

Parameters sit inside braces, before the `in` keyword. Swift is the second language here with a trailing-lambda syntax (https://docs.swift.org/swift-book/documentation/the-swift-programming-language/closures#Trailing-Closure-Syntax).

### Rust

| form | example |
|---|---|
| no parameters | `|| 42` |
| one parameter | `|x| x * 2` |
| several parameters | `|x, y| x + y` |
| typed parameter | `|x: i32| x * 2` |
| block body | `|x| { x * 2 }` |
| trailing lambda | none — the last argument stays inside the parentheses |

Pipes delimit the parameter list. The empty pair `||` is the no-parameter form. Rust is the direct ancestor of Vyrn's current syntax (https://doc.rust-lang.org/reference/expressions.html#closure-expressions).

### Go

| form | example |
|---|---|
| no parameters | `func() int { return 42 }` |
| one parameter | `func(x int) int { return x * 2 }` |
| several parameters | `func(x, y int) int { return x + y }` |
| typed parameter | `func(x int) int { return x * 2 }` (types are required) |
| block body | `func(x int) int { return x * 2 }` (always a block) |
| trailing lambda | none — the last argument stays inside the parentheses |

Go has no expression body and no type inference at the literal. The literal is a full function declaration in miniature (https://go.dev/ref/spec#Function_literals).

### TypeScript

| form | example |
|---|---|
| no parameters | `() => 42` |
| one parameter | `x => x * 2` |
| several parameters | `(x, y) => x + y` |
| typed parameter | `(x: number) => x * 2` |
| block body | `(x) => { return x * 2; }` |
| trailing lambda | none — the last argument stays inside the parentheses |

A single parameter may drop the parentheses. The arrow is `=>`. The grammar is ECMA-262 §15.3 (https://tc39.es/ecma262/#sec-arrow-function-definitions).

### Python

| form | example |
|---|---|
| no parameters | `lambda: 42` |
| one parameter | `lambda x: x * 2` |
| several parameters | `lambda x, y: x + y` |
| typed parameter | `lambda x: x * 2` (no type annotations on lambda parameters) |
| block body | none — the body is a single expression |
| trailing lambda | none — the last argument stays inside the parentheses |

The `lambda` keyword opens the literal. No types, no block, no statements (https://docs.python.org/3/reference/expressions.html#lambda).

### Ruby

| form | example |
|---|---|
| no parameters | `-> { 42 }` or `lambda { 42 }` |
| one parameter | `->(x) { x * 2 }` or `lambda { |x| x * 2 }` |
| several parameters | `->(x, y) { x + y }` or `lambda { |x, y| x + y }` |
| typed parameter | none (Ruby is dynamically typed) |
| block body | `->(x) { puts x; x * 2 }` (last expression is the result) |
| trailing lambda | `foo(a) { |x| x * 2 }` — a block is the trailing-lambda form |

Ruby has two literal spellings: the stabby `->(...) { }` and `lambda { || }`. A method call may take a block after the argument list, which is the trailing-lambda equivalent (https://docs.ruby-lang.org/en/master/syntax/methods_rdoc.html).

### Elixir

| form | example |
|---|---|
| no parameters | `fn -> 42 end` |
| one parameter | `fn x -> x * 2 end` |
| several parameters | `fn x, y -> x + y end` |
| typed parameter | none (Elixir is dynamically typed) |
| block body | `fn x -> x * 2 end` (multiple clauses; the body is a sequence) |
| trailing lambda | none — the `&(&1 * 2)` shorthand is the closest form |

The `fn` keyword opens the literal and `end` closes it. The arrow is `->`. A literal may carry several clause heads (https://hexdocs.pm/elixir/function-capture-and-anonymous-functions.html).

### OCaml

| form | example |
|---|---|
| no parameters | `fun () -> 42` |
| one parameter | `fun x -> x * 2` |
| several parameters | `fun x y -> x + y` |
| typed parameter | `fun (x: int) -> x * 2` |
| block body | `fun x -> let r = x * 2 in r` |
| trailing lambda | `foo a (fun x -> x * 2)` — no special trailing syntax |

The `fun` keyword opens the literal. Parameters are space-separated. The arrow is `->` (https://v2.ocaml.org/manual/expr.html).

### Haskell

| form | example |
|---|---|
| no parameters | `\_ -> 42` (Haskell has no zero-argument lambda; `()` is the unit argument) |
| one parameter | `\x -> x * 2` |
| several parameters | `\x y -> x + y` |
| typed parameter | `\x -> x * 2 :: Int` (no inline parameter annotation; the type is on the body) |
| block body | `\x -> do { r <- pure (x * 2); pure r }` (a `do` block) |
| trailing lambda | `foo a $ \x -> x * 2` — the `$` operator lets the lambda run to the right without parentheses |

The backslash opens the literal. The arrow is `->` (https://www.haskell.org/onlinereport/haskell2010/haskellch3.html#x8-470003).

### Zig

Zig has no lambda literal. A first-class function value is a struct with a `fn` field or a function pointer. The closest form is a struct literal whose field names a declared function. There is no inline expression body (https://ziglang.org/documentation/master/#Functions).

### Nim

| form | example |
|---|---|
| no parameters | `() => 42` or `proc(): int = 42` |
| one parameter | `(x) => x * 2` |
| several parameters | `(x, y) => x + y` |
| typed parameter | `(x: int) => x * 2` or `proc(x: int): int = x * 2` |
| block body | `proc(x: int): int = (echo x; x * 2)` or a statement list |
| trailing lambda | none — the last argument stays inside the parentheses |

Nim has two spellings: the arrow form `(params) => body` and the long form `proc(params): ret = body` (https://nim-lang.org/docs/manual.html#procedures-anonymous-procs).

### Gleam

| form | example |
|---|---|
| no parameters | `fn() { 42 }` |
| one parameter | `fn(x) { x * 2 }` |
| several parameters | `fn(x, y) { x + y }` |
| typed parameter | none (Gleam is statically typed but infers; no inline annotation) |
| block body | `fn(x) { x * 2 }` (always a block) |
| trailing lambda | `foo(a, fn(x) { x * 2 })` — no special trailing syntax |

The `fn` keyword opens the literal. The body is always a brace block. There is no expression body (https://tour.gleam.run/basics/functions/).

## The parsing conflicts

This is the part that matters. Each candidate syntax has a conflict in a language that also has Vyrn's features. Vyrn has: a return-type arrow `->` (`fn(Int64) -> Int64`), match arms with `=>` (`Some(a) => a`), bitwise-or `|` as an infix operator (RFC-0045), enum variants introduced by a leading `|` on a type RHS (`type Shape = | Circle(Int) | Rect(Int, Int)`), brace blocks and record literals (`{ ... }`), and the keyword `fn`.

### `->` arrow (Java, OCaml, Haskell, Elixir, Scala)

**Conflict:** `->` already means "return type" in Vyrn. A `fn`-typed parameter is `fn(Int64) -> Int64`, and a function declaration is `fn f(x: Int64) -> Int64`. Reusing `->` for a lambda body puts two meanings on one token.

**How the shipping language resolves it:** Java requires a parameter list (possibly empty `()`, or a bare identifier) before the `->`. The parser knows it is in a lambda because the `->` follows something that parsed as a parameter list, not a type. The JLS grammar (§15.27) makes `LambdaParameters` a distinct non-terminal from `Expression`, so the arrow is never ambiguous in the grammar — but a hand-written parser still needs a speculative parse of the parenthesised group to tell `(x)` the parameter list from `(x)` the parenthesised expression (JLS §15.27, https://docs.oracle.com/javase/specs/jls/se22/html/jls-15.html#jls-15.27). OCaml and Haskell have no return-type arrow in their surface syntax (types are inferred or annotated on the body), so `->` is free for lambda. Elixir uses `->` only inside `fn..end` and inside `cond`/`case` clauses, where the surrounding `fn`/`end`/`do`/`end` keywords bound it. Scala has no `->` return-type arrow (it uses `: Type =`), so `=>` is free — but Scala 3 also uses `=>` for context-function types and for `match` arms, and resolves by context.

**In Vyrn:** a `->`-bodied lambda needs a marker that says "this is a parameter list, not a type." Without one, `|x| -> x` versus `(x) -> x` versus `fn(Int64) -> Int64` collide.

### `=>` arrow (C\#, Kotlin, Scala, Nim, TypeScript)

**Conflict:** `=>` is Vyrn's match-arm token (`Some(a) => a`). A match arm whose body is itself a lambda would put two `=>` on one line. C\# faces the same conflict: C\# switch expressions use `=>` for arms (`case 1 => "one"`) and `=>` for lambdas. The C\# parser resolves by context — a switch expression starts with `switch` and is enclosed in braces, so the `=>` inside is an arm, while a `=>` outside is a lambda (C\# spec §11.19, §12.21). TypeScript has no match arm with `=>` (uses `:` in switch), so it is free. Kotlin uses `->` in `when` arms, not `=>`, so `=>` is free for lambdas in Kotlin.

**In Vyrn:** the match arm and the lambda would share `=>`. The parser can resolve by context (a `=>` at match-arm depth is an arm; a `=>` in argument or `let` position is a lambda), but a lambda as a match-arm body is exactly the case that needs care.

### `(x) -> x` parenthesised form (Java)

**Conflict:** arbitrary lookahead. A parenthesised group `(x)` is a valid expression (a parenthesised variable read). The parser cannot tell whether `(x)` is a lambda parameter list or a parenthesised expression until it sees the token after the closing `)` — `->` means lambda, anything else means expression. This is a speculative parse or a backtrack.

**How Java resolves it:** the JLS grammar makes `LambdaParameters` a distinct non-terminal, and a hand-written parser (javac) does a bounded lookahead: after `)`, peek for `->`. If found, reinterpret the group as parameters; if not, it was a parenthesised expression. The cost is one token of lookahead and a re-parse of the group contents (JLS §15.27).

**In Vyrn:** the current recursive-descent parser (`compiler/vyrn-frontend/src/parser.rs:4482`) parses a primary and then applies postfix operators. A `(x) -> x` form would need the primary parser to speculatively treat `(...)` as a parameter list when `->` follows, which is a structural change to `primary`.

### `|x|` pipe form (Rust, current Vyrn)

**Conflict:** `|` is Vyrn's infix bitwise-or operator (RFC-0045, `compiler/vyrn-frontend/src/parser.rs:4110`). It also introduces enum variants on a type RHS (`compiler/vyrn-frontend/src/parser.rs:2625`).

**How Vyrn resolves it today:** `|` is infix-only — it never starts an expression. The parser's `primary` (`compiler/vyrn-frontend/src/parser.rs:4485`) checks for `Tok::Pipe` or `Tok::OrOr` at the start of a primary and dispatches to `lambda` (`compiler/vyrn-frontend/src/parser.rs:4451`). A `|` in infix position is a binary operator. A `|` on a type RHS is an enum variant. The three contexts do not overlap because a type RHS is not an expression context and an infix operator is never a primary. The formatter makes the same distinction by tracking `in_lambda_params` and `in_type_decl` (`compiler/vyrn-frontend/src/fmt.rs:142`, `compiler/vyrn-frontend/src/fmt.rs:146`).

**Cost of the conflict:** the conflict is already paid for. The pipe form works because `|` is infix-only. The RFC-0045 note in RFC-0023 records this (`rfcs/RFC-0023-function-values.md:137`).

### `{ x -> x }` or `{ x in x }` brace form (Kotlin, Swift)

**Conflict:** `{ ... }` is a block and a record literal in Vyrn. A brace form for a lambda puts a parameter list and an arrow or `in` keyword inside a block-shaped delimiter. The parser must tell a block `{ stmt; stmt }` from a lambda `{ x -> x }` from a record literal `{ name: Int }`.

**How Kotlin resolves it:** a `{ ... }` in expression position is a lambda; the `->` inside separates parameters from body. A record literal is not a Kotlin expression (Kotlin uses `DataClass(...)`), so the conflict does not arise. Swift uses the `in` keyword inside braces; a struct literal is not a Swift expression either. Both languages avoid the conflict by not having brace-delimited record literals in expression position.

**In Vyrn:** Vyrn has record literals (`User { name: "x" }`) and blocks. A brace lambda would need a marker (`->` or `in`) inside the braces to separate it from a block or a record, and the parser would need to peek past the first token.

### `fn(x) { }` keyword form (Gleam, Go-adjacent)

**Conflict:** `fn` is already a Vyrn keyword, used for declarations (`fn f(x: Int64) -> Int64`). A `fn(x)` literal in expression position reuses the keyword.

**How Gleam resolves it:** Gleam has no `fn name(...)` declaration form — a top-level function is `fn f(x) { ... }` and a literal is `fn(x) { ... }`. The presence or absence of a name after `fn` tells declaration from literal. Go uses a distinct keyword `func` for both, and the literal `func(x int) int { ... }` is always a full signature.

**In Vyrn:** `fn` is followed by a name in a declaration and by `(` in a literal. The parser already distinguishes `fn name(...)` from other forms at `compiler/vyrn-frontend/src/parser.rs:1289`. A `fn(x)` literal would be unambiguous: `fn` followed by `(` in expression position is a lambda, `fn` followed by an identifier in declaration position is a declaration. No new token, no lookahead, no backtrack. The cost is verbosity — the literal is as long as a declaration.

## The work counts

Run with `bash` + `grep` (the built-in `grep` tool does not pipe to `wc -l`):

```
$ grep -rn '|[a-zA-Z_]' --include=*.vyrn std/ examples/ site/ | wc -l
65842

$ grep -rln '|[a-zA-Z_]' --include=*.vyrn . | wc -l
17466
```

The pattern `|[a-zA-Z_]` matches a pipe followed by a letter or underscore. It catches three things, not only lambdas: a lambda open (`|x|`), an enum variant lead (`| Circle(Int64)`), and a bitwise-or with no space (`a|b`). It does not match spaced bitwise-or (`a | b`). The first count (65842 lines) is over `std/`, `examples/`, and `site/`. The second count (17466 files) is over the whole working tree, which includes `.claude/worktrees/` clones; the live source outside the worktrees is 315 files in `std/`, `examples/`, and `site/` that contain a match.

NOT MEASURED: how many of the 65842 lines are lambdas specifically, versus enum variants or bitwise-or. A narrower pattern that excludes enum leads (a `|` after `=` on a type RHS) would need a multi-line match. The count above is the one the task asked for, run as asked.

## Where the current syntax is produced or consumed

Every place the `|x|` / `||` lambda syntax is read, written, checked, formatted, highlighted, or documented. Citations are `path:LINE`.

### Lexer — produces the tokens

- `Tok::Pipe` declared: `compiler/vyrn-frontend/src/lexer.rs:93`
- `Tok::OrOr` declared (the zero-parameter `||`): `compiler/vyrn-frontend/src/lexer.rs:86`
- `two_char_op` maps `('|','|')` to `OrOr`: `compiler/vyrn-frontend/src/lexer.rs:277`
- `single_char_op` maps `'|'` to `Pipe`: `compiler/vyrn-frontend/src/lexer.rs:308`
- The `lex` function's single-char arm: `compiler/vyrn-frontend/src/lexer.rs:1186`
- `token_name_and_text` spells `Pipe` as `("punct", "|")` and `OrOr` as `("punct", "||")`: `compiler/vyrn-frontend/src/lexer.rs:186`, `compiler/vyrn-frontend/src/lexer.rs:182`

The lexer is pipe-blind. It emits `Pipe` or `OrOr` and does not know whether a `|` opens a lambda, separates enum variants, or is a bitwise-or. That decision is the parser's and the formatter's.

### Parser — consumes the tokens

- `lambda()` parses `|x| expr`, `|x, y| { block }`, and `|| expr`: `compiler/vyrn-frontend/src/parser.rs:4451` (the function body runs to `compiler/vyrn-frontend/src/parser.rs:4479`)
- `primary()` dispatches a bare `|` or `||` to `lambda`: `compiler/vyrn-frontend/src/parser.rs:4485`–`compiler/vyrn-frontend/src/parser.rs:4490`
- `enum_type()` consumes a leading `|` as an enum variant separator on a type RHS: `compiler/vyrn-frontend/src/parser.rs:2625`–`compiler/vyrn-frontend/src/parser.rs:2647`
- `binop` maps `Tok::Pipe` to `BitOr` at precedence 6: `compiler/vyrn-frontend/src/parser.rs:4110`
- Parser tests: `parses_expression_lambda` at `compiler/vyrn-frontend/src/parser.rs:6720`, `parses_block_and_multiparam_and_niladic_lambda` at `compiler/vyrn-frontend/src/parser.rs:6735`, `lambda_body_precedence_spans_or` at `compiler/vyrn-frontend/src/parser.rs:6759`

### AST — the representation

- `Expr::Lambda { params, body, line }`: `compiler/vyrn-frontend/src/ast.rs:1314`–`compiler/vyrn-frontend/src/ast.rs:1318`
- `LambdaBody { Expr, Block }`: `compiler/vyrn-frontend/src/ast.rs:1337`–`compiler/vyrn-frontend/src/ast.rs:1340`
- `lambdas(p: &Program)` collects every lambda literal by node address (RFC-0101 M6): `compiler/vyrn-frontend/src/ast.rs:1422`

### Checker — type-checks the literal

- `check_fn_arg` handles a lambda argument in a `fn`-typed parameter position: `compiler/vyrn-frontend/src/checker.rs:8155`
- `stored_fn_lambda` checks a lambda flowing into a stored function value (RFC-0037): `compiler/vyrn-frontend/src/checker.rs:8374`
- A lambda in a position with no function type is rejected: `compiler/vyrn-frontend/src/checker.rs:4689`–`compiler/vyrn-frontend/src/checker.rs:4694`
- Capture discipline (read-only, no assign/drop/consume): `compiler/vyrn-frontend/src/checker.rs:8594`–`compiler/vyrn-frontend/src/checker.rs:8610`
- Nested-literal lock: `compiler/vyrn-frontend/src/checker.rs:8810`–`compiler/vyrn-frontend/src/checker.rs:8813`
- Checker tests: `accepts_lambda_and_named_fn_argument` at `compiler/vyrn-frontend/src/checker.rs:13141`, `lambda_still_needs_a_function_type_from_context` at `compiler/vyrn-frontend/src/checker.rs:13266`, `lambda_arity_mismatch_is_rejected` at `compiler/vyrn-frontend/src/checker.rs:13278`, `nested_lambda_literal_is_rejected` at `compiler/vyrn-frontend/src/checker.rs:13341`

### Formatter — `vyrn fmt`

- `lambda_open` and `lambda_close` role fields: `compiler/vyrn-frontend/src/fmt.rs:64`–`compiler/vyrn-frontend/src/fmt.rs:69`
- The `Tok::Pipe` role assignment (open vs close vs enum vs bitwise): `compiler/vyrn-frontend/src/fmt.rs:232`–`compiler/vyrn-frontend/src/fmt.rs:257`
- The space rule for tight lambda pipes: `compiler/vyrn-frontend/src/fmt.rs:283`–`compiler/vyrn-frontend/src/fmt.rs:288`
- Indent for a leading `|` (enum-variant style): `compiler/vyrn-frontend/src/fmt.rs:345`–`compiler/vyrn-frontend/src/fmt.rs:352`
- Formatter tests: `lambdas_and_fn_types` at `compiler/vyrn-frontend/src/fmt.rs:550`, `leading_pipe_enum_indents` at `compiler/vyrn-frontend/src/fmt.rs:742`

### Interpreter — runs the literal

- `FnVal::Lambda { params, body, captures, param_tys, ret }`: `compiler/vyrn-frontend/src/interp.rs:813`
- `make_closure` snapshots captures at the evaluation site: `compiler/vyrn-frontend/src/interp.rs:2687`
- `eval_fn_arg` materialises a lambda argument: `compiler/vyrn-frontend/src/interp.rs:2742`
- `call_fnval` invokes a lambda value: `compiler/vyrn-frontend/src/interp.rs:2766`
- A bare lambda in a storage position: `compiler/vyrn-frontend/src/interp.rs:4233`

### Lowering — `vyrn-lower`

- Imports `LambdaBody`: `compiler/vyrn-lower/src/lib.rs:36`
- `lambda_bodies` set (every block that is a lambda body): `compiler/vyrn-lower/src/lib.rs:303`
- The `Expr::Lambda` walk arm: `compiler/vyrn-lower/src/lib.rs:840`–`compiler/vyrn-lower/src/lib.rs:846`

### Codegen — `vyrn-codegen`

- `direct.rs` uses `ast::lambdas(program)` to hand back borrows: `compiler/vyrn-codegen/src/direct.rs:378`
- `Key::Lambda(usize, Vec<Type>, Vec<(String, Type)>)` is a lifted-lambda worklist key: `compiler/vyrn-codegen/src/direct.rs:855`
- `Body::Shell` is a lifted lambda whose literal the program does not hold: `compiler/vyrn-codegen/src/direct.rs:874`
- `lib.rs` `lambda_defs` and `lambda_emitted` dedup: `compiler/vyrn-codegen/src/lib.rs:2396`, `compiler/vyrn-codegen/src/lib.rs:1753`
- `drain_ho` appends each lifted-lambda definition once: `compiler/vyrn-codegen/src/lib.rs:661`

### LSP — `vyrn-lsp`

The LSP has no lambda-specific code. It does not parse, format, or special-case `|`. Diagnostics, hover, completion, and rename all run through the shared frontend (`compiler/vyrn-lsp/src/main.rs`). A grep for `lambda` or `Pipe` in `compiler/vyrn-lsp/src/` finds no matches. A syntax change touches the LSP only through the frontend it shares.

### Site syntax highlighting — `site/app/hl.vyrn`

The site highlighter colours by the lexer's token kind. `classOf` (`site/app/hl.vyrn:315`) returns a CSS class for `keyword`, `string`, `int`, `ident`, `doc`. It returns `""` for `punct`, so `|` and `||` get no colour. There is no lambda-specific rule and no lambda snippet in the `pieces()` array (`site/app/hl.vyrn:42`). A syntax change touches this file only if a new token kind is added; a re-spelling of `|` to a keyword like `fn` would make `classOf` colour it as a keyword automatically.

### Editor extension — `editor/vscode/`

- The TextMate grammar matches `|` as a generic operator: `editor/vscode/vyrn.tmLanguage.json:163` (`"match": "->|=>|==|!=|<=|>=|[+\\-*/%=<>!?|&]"`). There is no lambda-specific rule. A re-spelling to a keyword would be picked up by the keyword rule at `editor/vscode/vyrn.tmLanguage.json:136`.
- `extension.js` is a thin LSP client (`editor/vscode/extension.js:1`). It has no syntax logic.
- The snippets file has no lambda snippet: `editor/vscode/snippets/vyrn.json`. The only `|`-using snippet is the enum (`editor/vscode/snippets/vyrn.json:30`).
- `language-configuration.json` has no bracket-pair entry for `|` (it is not a bracket).

### Guide — `site/guide/`

- `lambdas.vyrn`: the `|x|` and `|acc, x|` examples: `site/guide/lambdas.vyrn:13`, `site/guide/lambdas.vyrn:14`
- `closures.vyrn`: the `|x|` examples in a stored-closure context: `site/guide/closures.vyrn:8`, `site/guide/closures.vyrn:12`

### Documentation — `docs/api/`

The API docs show `fn`-typed parameters (the positions a lambda fills), not lambda literals:
- `arrays.md`: `map`, `filter`, `fold`, `any`, `all`, `sortBy` signatures: `docs/api/std/arrays.md:11`, `docs/api/std/arrays.md:19`, `docs/api/std/arrays.md:27`, `docs/api/std/arrays.md:35`, `docs/api/std/arrays.md:43`, `docs/api/std/arrays.md:64`
- `bench.md`: `benchMeasure`, `benchOne` take `fn()` bodies: `docs/api/std/bench.md:70`, `docs/api/std/bench.md:89`
- `http.md`, `stream.md`, `ui.md`, `graphql.md`: `fn(..)`-typed fields and parameters throughout

The docs do not spell out the `|x|` literal. A syntax change touches the docs only where a doc example shows a lambda inline (none found in `docs/api/`; the guide is the only prose with literal examples).

### RFCs

- RFC-0023 defines the surface: `rfcs/RFC-0023-function-values.md:127`–`rfcs/RFC-0023-function-values.md:131`
- The RFC-0045 correction (there IS a bitwise-or, and the rule still works): `rfcs/RFC-0023-function-values.md:133`–`rfcs/RFC-0023-function-values.md:141`
- RFC-0037 lifts the parameter-only restriction and reuses the literal: `rfcs/RFC-0037-stored-closures.md:4`

## What Vyrn has today

The lambda literal is `|x| expr`, `|x, y| { block }`, and `|| expr` for zero parameters (`compiler/vyrn-frontend/src/parser.rs:4451`). The parameters are untyped names. Their types flow from the expected `fn(..) -> R` type at the checker (`compiler/vyrn-frontend/src/checker.rs:8155`). A block body uses `return` like a function body; an expression body is the returned value directly (`compiler/vyrn-frontend/src/ast.rs:1337`).

The lexer emits `Tok::Pipe` for `|` and `Tok::OrOr` for `||` (`compiler/vyrn-frontend/src/lexer.rs:93`, `compiler/vyrn-frontend/src/lexer.rs:86`). It does not distinguish lambda pipes from bitwise-or pipes from enum-variant pipes. The parser does, by position: a `|` at the start of a primary is a lambda (`compiler/vyrn-frontend/src/parser.rs:4485`); a `|` in infix position is `BitOr` (`compiler/vyrn-frontend/src/parser.rs:4110`); a `|` on a type RHS is an enum variant (`compiler/vyrn-frontend/src/parser.rs:2625`). The formatter makes the same distinction with an `in_lambda_params` flag and an `in_type_decl` flag (`compiler/vyrn-frontend/src/fmt.rs:142`, `compiler/vyrn-frontend/src/fmt.rs:146`).

The literal is legal in two positions: a `fn`-typed call argument (RFC-0023) and any storage position with a `fn` type — a `let`, a return, a record field, an array element, an `Option` payload, module state (RFC-0037). A lambda with no `fn` type in context is a checker error with a message that suggests the annotation (`compiler/vyrn-frontend/src/checker.rs:4689`). A lambda body may not contain another lambda literal (`compiler/vyrn-frontend/src/checker.rs:8810`).

Every lambda literal is monomorphized away. It lifts to a top-level function `@__vyrn_lambda_<fn>_<ordinal>_<shape>_h<sha256/16>` whose leading parameters are its captures (`rfcs/RFC-0023-function-values.md:185`). No function pointer survives to run time in any backend. The interpreter snapshots captures by value at the evaluation site (`compiler/vyrn-frontend/src/interp.rs:2687`); the compiled backends do the same at the outer call site.

What would have to change for a new syntax: the `lambda()` parser, the `primary()` dispatch, the formatter's pipe-role logic (`compiler/vyrn-frontend/src/fmt.rs:232`), the two guide files, and — if the new syntax uses a keyword — the keyword table (`compiler/vyrn-frontend/src/lexer.rs:236`) and the TextMate keyword rule (`editor/vscode/vyrn.tmLanguage.json:136`). The checker, the interpreter, the lowering, and the codegen do not touch the surface syntax; they work on `Expr::Lambda`, which stays the same. The AST node (`compiler/vyrn-frontend/src/ast.rs:1314`) needs no change for any candidate that keeps untyped parameters.

## The options

RECOMMENDATION, NOT A DECISION.

Five designs. Each row says what the design is, what it costs in the parser, the checker, lowering, what breaks in existing code, and who else does it.

| design | one-sentence description | parser cost | checker cost | lowering cost | what breaks in existing code | who else does it |
|---|---|---|---|---|---|---|
| pipe form `|x| x` (status quo) | keep `|x| expr`, `|| expr` | none — already shipped | none | none | nothing | Rust, Ruby block |
| `fn(x) { x }` keyword form | a `fn`-keyword literal: `fn(x) { x * 2 }`, `fn() { 42 }`, `fn(x, y) { x + y }` | low — `fn` followed by `(` in expression position is a lambda; `fn` followed by a name is a declaration, already distinguished at `parser.rs:1289` | none — `Expr::Lambda` is unchanged | none | every `.vyrn` file that writes `|x|` today (69 lambda sites, 30 files in `std/`/`examples/`/`site/`) needs a rewrite; the formatter, the guide, the snippets, and the TextMate grammar all change | Gleam, Go (with `func`) |
| `->` arrow form `(x) -> x` | a Java-style arrow: `(x) -> x * 2`, `() -> 42`, `(x: Int64) -> x * 2` | medium — `primary` must speculatively parse `(...)` as a parameter list when `->` follows the `)`, which is a backtrack or a bounded lookahead; `->` already means return type, so the parser needs a context flag to separate a lambda arrow from a type arrow | none — `Expr::Lambda` is unchanged; a typed parameter would need a new `Param`-like field or stay untyped | none | every `.vyrn` file; plus the `->` return-type arrow and the lambda arrow share a token, so diagnostics that mention `->` need to say which | Java, OCaml, Haskell, Elixir, Scala |
| `=>` arrow form `(x) => x` | a C\#-style arrow: `(x) => x * 2`, `() => 42` | medium — same parenthesised-list lookahead as the `->` form; `=>` is the match-arm token, so a lambda as a match-arm body is the case that needs care | none | none | every `.vyrn` file; `=>` now means both match arm and lambda, so a match arm whose body is a lambda reads `Some(a) => (x) => x` | C\#, TypeScript, Kotlin, Scala, Nim |
| brace form `{ x -> x }` | a Kotlin/Swift-style brace literal: `{ x -> x * 2 }`, `{ -> 42 }`, `{ x: Int64 -> x * 2 }` | high — `{ ... }` is already a block and a record literal; the parser must peek past the first token to tell a lambda brace from a block or a record, and a trailing-lambda rule (last arg moves outside parens) is a structural change to call parsing | none | none | every `.vyrn` file; the parser change is the largest of the five because it reuses the block delimiter | Kotlin, Swift |

A trailing-lambda syntax (last argument moves outside the parentheses) is orthogonal to the arrow choice. Kotlin and Swift ship it; Rust, Java, C\#, Go, TypeScript, Python, OCaml, Haskell, Gleam do not. It would be a separate change to call parsing under any of the five designs, and it is the one feature that would make a new syntax more than cosmetic.


---

## Correction, made on verification

The three counts above were wrong. They swept `.claude/worktrees/`, which holds
about 70 full clones of this repository, and they reported "files searched" as
"files containing a match".

Verified numbers, `std/` and `examples/` and `site/` only:

| what | claimed | verified | command |
| --- | --- | --- | --- |
| lines matching `\|[a-zA-Z_]` | 65842 | **83** | `grep -rn '\|[a-zA-Z_]' --include=*.vyrn std/ examples/ site/ \| wc -l` |
| files containing a match | 17466, then 315 | **30** | `grep -rln '\|[a-zA-Z_]' --include=*.vyrn std/ examples/ site/ \| wc -l` |
| lambda sites specifically | NOT MEASURED | **69** | `grep -rEn '\|[a-zA-Z_][a-zA-Z0-9_]*(, *[a-zA-Z_][a-zA-Z0-9_]*)*\|' --include=*.vyrn std/ examples/ site/ \| wc -l` |

315 is the total number of `.vyrn` files in those three directories. 17466 is
the total number of `.vyrn` files in the working tree including the clones.
Neither counts a match.

This changes what the census concludes. A migration touching 69 sites across 30
files is cheap. The tables above cost the change as if it touched thousands.
Re-read every "what breaks in existing code" cell against 69, not 65842.

---

## Correction, after the change was made

**Decided and shipped as RFC-0110: `x -> e`, `(a, b) -> e`, `() -> e`.**

Two things this file got wrong, both found by building it.

**The `->` conflict is not real.** This file says a `->`-bodied lambda "needs a
marker that says this is a parameter list, not a type", because `->` already
means return type. The two arrows never share a context: a return type is
written where a TYPE is expected and a lambda where an EXPRESSION is expected,
and no position in the grammar takes both. The type parser never calls the
expression parser. No marker was needed, and no backtracking: one token of
lookahead for `x ->`, and for `(a, b) ->` a scan of a list that is names and
commas and nothing else.

The survey rows are accurate about the other languages. What did not survive was
the inference from Java's grammar to Vyrn's parser — Java needs a speculative
parse of `(x)` because `(x)` is a tuple-ish expression there, and in Vyrn it
cannot be one.

**The work count was wrong by three orders of magnitude.** This file first said
65,842 lines across 17,466 files. The real figure is 69 sites in 30 files, and
the migration touched 62 of them plus the Vyrn fixtures embedded in Rust test
sources. The cause is recorded above: the greps swept `.claude/worktrees/`,
which holds about seventy full copies of the repository.

The rest of the survey stands, and the `fn(x) { }` row — the one candidate this
file found needs no lookahead at all — remains the honest runner-up.
