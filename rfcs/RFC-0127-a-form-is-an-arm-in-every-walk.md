# RFC-0127 — a form is an arm in every walk

- **Status:** Census closed (2026-09-05). The table is the deliverable and
  `compiler/vyrn-cli/tests/forms.rs` pins it. §5 ranks five collapses and records
  the number that stopped four of them; §8 takes the fifth — `test` and `bench`
  are one carrier now, and the surface keeps both words. Two defects it found are
  fixed
  in §6: five doc comments in `ast.rs` describe a surface the code stopped
  having, and the editor grammar colours `yield`, a word with no mention in any
  column of §3.4.
- **Depends on:** RFC-0126, which asked this question of `ast::Type` and whose
  method §2 reuses without a change; RFC-0125 §1.1, which names `surface` as a
  factor of the size and does not price it; RFC-0094, the same shape asked of
  the 107 reserved names; RFC-0118, the same shape asked of one form and taken.
- **Evidence:** the counts in §3, measured at this branch's tip by the method in
  §2 and recomputed by `tests/forms.rs` on every run; the two emitted-IR probes
  in §5.4; `editor/vscode/test/grammar.test.mjs` and
  `compiler/vyrn-cli/tests/contextual_words.rs`, which already hold two other
  copies of the keyword and contextual lists against the lexer.

---

## 1. The question

RFC-0126 measured one factor of RFC-0125 §1.1's `(surface × types × builtins ×
engines)` and left the rest. `types` was 35 constructors and is 33 now. This
RFC measures `surface`: every statement, every expression, every pattern, every
declaration, and every keyword and contextual word the lexer and the parser
recognise. 86 rows, one verdict each.

The brief that asked for it stated a hypothesis about desugars:

> the parser turns `??`, `?`, `if let`, refutable `let`, `x -> e` into a `match`

**Two of the five, and two it does not name.** `??` becomes a `match` with
`Pattern::Success` and `Pattern::Failure` (RFC-0079), and a refutable `let`
becomes a `match` with `Pattern::Other` (RFC-0121). The other three are nodes of
their own: `?` is `Expr::Try`, `if let` is `Stmt::IfLet`, and `x -> e` is
`Expr::Lambda` — a spelling, not an expansion, and RFC-0110's only change to the
node was which characters reach it. What the parser does rewrite, and the brief
does not name, is `while let`, which becomes `while true { if let .. else
break }`, and `else if`, which becomes a nested `Stmt::If`.

Treating that paragraph as a finding would have priced two free collapses. The
same thing happened to RFC-0126 §2.8 and the same sentence applies: a hypothesis
about the surface is worth what a measurement of it costs, which is one test.

---

## 2. The method, so a number can be re-derived

RFC-0126 §2's rule, unchanged, and `tests/forms.rs` applies exactly it:

1. A line whose trimmed text starts with `//` is skipped, so a doc comment that
   names a form is not a case.
2. An item annotated `#[cfg(test)]` is skipped whole. A fixture in a test module
   is not a pass.
3. In what is left, the needle counts where the next character is not a letter,
   digit or underscore. That is what keeps `Stmt::If` from counting
   `Stmt::IfLet`, and `.tests` from counting `.tests_run`.

**A mention, not an arm**, for RFC-0126 §2's reason: counting arms needs a Rust
parser and under-reports a form the parser BUILDS in eight places. The unit is
"the form is named here".

Four tables, because the surface is not one kind of thing and one needle would
lie about three of them:

| table | row | needle | why that needle |
|---|---|---|---|
| §3.1 forms | a `Stmt`, `Expr` or `Pattern` variant | `Stmt::Let` | the variant is what a pass matches |
| §3.2 declarations | a `Vec` field of `Program` | `.functions` | a pass reaches a declaration through the field, not through the struct |
| §3.3 keywords | a `keyword_or_ident` arm | `Tok::Let` | the token is what the parser peeks at |
| §3.4 contextual words | a word the lexer returns as an identifier | `"consume"` | there is no token; the parser compares the string |

The declaration needle is the one that needed a measurement to choose. Counted
by struct name (`TypeDecl`, `ImportDecl`) the nine declarations come to 171
mentions and `import` scores 4, which says an import is nearly free and is
false: the loader names `.imports` nineteen times. Counted by field they come to
393 and the loader's column is the largest, which is what a reader of
`loader.rs` sees. The struct name is the declaration's TYPE; the field is the
declaration's PLACE, and a pass works on places.

**The files, and why each.** A form's nine and a declaration's eight are not the
same set, because a declaration is linked by the loader and selected by the CLI,
and neither of those files ever matches a statement.

| column | file | what it decides |
|---|---|---|
| `parser` | `vyrn-frontend/src/parser.rs` | what the surface spells, and what it rewrites |
| `checker` | `vyrn-frontend/src/checker.rs` | what a program means |
| `movecheck` | `vyrn-frontend/src/movecheck.rs` | where a value is moved |
| `own` | `vyrn-frontend/src/own.rs` | where a value is released |
| `lower` | `vyrn-lower/src/lib.rs`, `core.rs`, `typed.rs` | the lowered form, RFC-0125's core, and the must-use judgment, which reads the tree a reader wrote (RFC-0125 §3 M3) |
| `native` | `vyrn-codegen/src/lib.rs` | the textual-IR emitter |
| `wasm` | `vyrn-codegen/src/direct.rs` | the direct wasm emitter |
| `interp` | `vyrn-frontend/src/interp.rs` | the interpreter's engine |
| `editor` | `vyrn-frontend/src/symbols.rs` | hover, completion and the outline |
| `loader` | `vyrn-frontend/src/loader.rs` | which declarations one program holds |
| `project` | `vyrn-frontend/src/project.rs` | the memoized per-program answers |
| `cli` | `vyrn-cli/src/main.rs` | which declarations a subcommand runs |
| `lexer` | `vyrn-frontend/src/lexer.rs` | which words are words |
| `fmt` | `vyrn-frontend/src/fmt.rs` | the canonical spelling |

**What this leaves out on purpose.** `BinOp`'s 18 variants and `UnOp`'s 3 are
inside `Expr::Binary` and `Expr::Unary` and are not forms; RFC-0045 priced the
five it added. `LambdaBody` and `ArmBody` are body shapes, not forms, and §5.2
prices the pair anyway because they are twins. `codec.rs`, `schema_reflect.rs`,
`consteval.rs` and `vyrn-genwasm` add about 300 more mentions; they are
downstream of the nine, as RFC-0126 §2 says of its own excluded thousand, and
adding them moves every row by roughly one factor without changing an order.
RFC-0126 §8.13 records what that exclusion cost once — a dead arm in the
excluded generation crate that no gate could see — so §8's gate list names
`genwasm` explicitly rather than trusting the workspace.

---

## 3. The census: what each form costs

### 3.1 The 38 forms

1,399 mentions in eight files for 38 forms. The table is generated by
`cargo test -p vyrn-cli --test forms -- --ignored --nocapture
the_form_census_as_a_table` and checked against the code by
`the_form_census_is_what_the_rfc_records`.

| form | parser | checker | movecheck | own | lower | shared | wasm | editor | all eight |
|---|---|---|---|---|---|---|---|---|---|
| `Stmt::Let` | 8 | 10 | 3 | 4 | 8 | 3 | 5 | 1 | 42 |
| `Stmt::Assign` | 2 | 6 | 2 | 3 | 7 | 3 | 3 | 1 | 27 |
| `Stmt::SetField` | 4 | 6 | 2 | 3 | 6 | 3 | 4 | 1 | 29 |
| `Stmt::IndexSet` | 4 | 6 | 2 | 3 | 6 | 3 | 3 | 1 | 28 |
| `Stmt::Return` | 1 | 12 | 2 | 3 | 8 | 5 | 4 | 3 | 38 |
| `Stmt::Break` | 2 | 6 | 1 | 3 | 8 | 3 | 2 | 1 | 26 |
| `Stmt::Continue` | 1 | 6 | 1 | 3 | 8 | 3 | 2 | 1 | 25 |
| `Stmt::If` | 2 | 9 | 4 | 3 | 8 | 3 | 2 | 1 | 32 |
| `Stmt::IfLet` | 2 | 7 | 4 | 3 | 8 | 3 | 3 | 1 | 31 |
| `Stmt::While` | 2 | 9 | 4 | 3 | 7 | 3 | 2 | 1 | 31 |
| `Stmt::ForIn` | 1 | 10 | 4 | 3 | 7 | 3 | 3 | 1 | 32 |
| `Stmt::Drop` | 1 | 7 | 2 | 3 | 6 | 3 | 3 | 1 | 26 |
| `Stmt::Expr` | 2 | 6 | 2 | 3 | 7 | 3 | 4 | 1 | 28 |
| `Stmt::Region` | 1 | 9 | 3 | 3 | 8 | 3 | 2 | 1 | 30 |
| `Expr::Int` | 4 | 9 | 7 | 2 | 8 | 2 | 4 | 2 | 38 |
| `Expr::Byte` | 2 | 7 | 5 | 2 | 8 | 2 | 4 | 2 | 32 |
| `Expr::Float` | 2 | 7 | 6 | 2 | 8 | 2 | 3 | 2 | 32 |
| `Expr::Bool` | 4 | 5 | 6 | 2 | 8 | 2 | 3 | 2 | 32 |
| `Expr::Str` | 7 | 6 | 6 | 4 | 8 | 2 | 8 | 3 | 44 |
| `Expr::Var` | 25 | 23 | 19 | 3 | 25 | 5 | 31 | 1 | 132 |
| `Expr::Unary` | 4 | 8 | 6 | 2 | 7 | 3 | 2 | 2 | 34 |
| `Expr::Binary` | 3 | 7 | 9 | 3 | 8 | 4 | 2 | 1 | 37 |
| `Expr::Call` | 22 | 13 | 29 | 4 | 26 | 4 | 12 | 1 | 111 |
| `Expr::Match` | 7 | 10 | 13 | 4 | 8 | 3 | 4 | 1 | 50 |
| `Expr::IfExpr` | 1 | 8 | 12 | 2 | 7 | 3 | 2 | 1 | 36 |
| `Expr::Try` | 1 | 7 | 6 | 2 | 6 | 3 | 4 | 1 | 30 |
| `Expr::StructLit` | 2 | 8 | 8 | 2 | 9 | 3 | 3 | 1 | 36 |
| `Expr::Field` | 6 | 7 | 16 | 3 | 14 | 3 | 3 | 1 | 53 |
| `Expr::TryConstruct` | 1 | 7 | 5 | 2 | 5 | 3 | 4 | 1 | 28 |
| `Expr::ArrayLit` | 5 | 7 | 6 | 2 | 9 | 4 | 4 | 2 | 39 |
| `Expr::MapLit` | 3 | 5 | 6 | 2 | 6 | 3 | 3 | 1 | 29 |
| `Expr::Spawn` | 1 | 7 | 5 | 2 | 7 | 3 | 5 | 1 | 31 |
| `Expr::Lambda` | 1 | 10 | 6 | 3 | 9 | 2 | 8 | 1 | 40 |
| `Expr::Consume` | 1 | 5 | 10 | 2 | 11 | 2 | 6 | 1 | 38 |
| `Pattern::Variant` | 11 | 5 | 2 | 0 | 1 | 1 | 6 | 1 | 27 |
| `Pattern::Success` | 1 | 5 | 3 | 0 | 3 | 1 | 5 | 1 | 19 |
| `Pattern::Failure` | 1 | 4 | 2 | 0 | 2 | 1 | 3 | 1 | 14 |
| `Pattern::Other` | 1 | 3 | 1 | 0 | 1 | 1 | 4 | 1 | 12 |

**The shape of this table is not RFC-0126's, and that is the finding.** The type
census ran from 7 mentions to 244, a factor of 35. This one runs from 12 to 132,
and once `Expr::Var` and `Expr::Call` are set aside — every walk names those two
for reasons of its own — from 12 to 53, with 33 of the 38 rows between 25 and
53. A type constructor's cost tells you something about the constructor. A
form's cost mostly does not.

### 3.1.1 The floor, and what is above it

`Stmt::Continue` costs 25 mentions. A `continue` has no operand, no type, no
value, no ownership effect and no diagnostic of its own; the checker asks
whether a loop encloses it and every other pass steps over it. So **25 is the
price of existing**, and the rows above it pay 25 plus what they decide:

| form | all eight | above the floor | what the excess buys |
|---|---|---|---|
| `Stmt::Continue` | 25 | 0 | the floor |
| `Stmt::Break` | 26 | 1 | one more arm, in `own` |
| `Stmt::Drop` | 26 | 1 | RFC-0114's explicit reclaim |
| `Stmt::Let` | 42 | 17 | a binder, a type, a placement, a copy rule |
| `Stmt::Return` | 38 | 13 | the release of every live binding on the way out |

An expression's floor is 28 (`Expr::TryConstruct`), and it is higher for the
same reason a `Stmt` floor exists: an expression is walked by more collectors
than a statement is. A pattern is walked by fewer than either, and its three
lowest rows — `Pattern::Other` at 12, `Pattern::Failure` at 14,
`Pattern::Success` at 19 — are the only rows under the statement floor.

**Where the floor comes from.** It is not the deciders. Taking `Stmt::Region`'s
30 apart: two mentions are the parser building and the checker's own rule; the
rest are exhaustive walks that do nothing with a region except recurse into its
block — `bound_names`, `scan_append_block`, `calls_block`, and a `line()` match
in each of four files. It was 33 over six such walks: `collect_regex_block`,
`collect_strings_block` and `mount_calls_block` belonged to the text-IR emitter
and went with it (RFC-0125 §3 M4). **About two thirds of every form's cost is walks
that do not decide anything.** That is the surface's real bill, and no collapse
in §5 touches it: deleting one form removes one arm from about twenty walks, and
the walks stay.

RFC-0125 §1.6 records the general shape of this — a fact restated in many places
drifts — and §7 says why pricing the walks is that RFC's milestone and not this
one's.

### 3.2 The 9 declarations

289 mentions in seven files. The rows are `Program`'s `Vec` fields, read out of
`ast.rs` by the test, so a tenth declaration form fails the census until it has
a row.

| declaration | parser | loader | checker | project | shared | editor | cli | all seven |
|---|---|---|---|---|---|---|---|---|
| `imports` | 1 | 19 | 0 | 0 | 0 | 5 | 1 | 26 |
| `type_decls` | 16 | 16 | 4 | 1 | 0 | 14 | 0 | 51 |
| `functions` | 2 | 18 | 22 | 1 | 1 | 12 | 15 | 71 |
| `protocols` | 1 | 11 | 7 | 0 | 0 | 8 | 0 | 27 |
| `contracts` | 0 | 11 | 4 | 0 | 0 | 0 | 0 | 15 |
| `impls` | 1 | 7 | 15 | 3 | 0 | 6 | 0 | 32 |
| `globals` | 1 | 12 | 14 | 1 | 2 | 2 | 0 | 32 |
| `tests` | 0 | 6 | 3 | 1 | 1 | 1 | 5 | 17 |
| `benches` | 0 | 6 | 3 | 1 | 1 | 1 | 6 | 18 |

`imports` is the only row with a zero in the checker, and that is RFC-0010
working: the loader consumes an import and the checker never sees one.
`contracts` is the only row that is zero in both emitters, and that is RFC-0071
working: a contract is comptime and reaches no engine.

**`tests` and `benches` differ by one mention**, and that one is the CLI, which
has two subcommands. Their carriers — `ast::TestDecl` and `ast::BenchDecl` — are
field for field the same struct, and `BenchDecl`'s own doc comment says so.
§5.1 prices the collapse and §8 takes it.

**A tenth declaration has no field.** `logging { level: .., sink: .. }` is a
declaration the parser reads and does not keep: it writes `Program::log_level`
and `Program::log_sink`, two scalars. That is why the derived row set is nine
and not ten, and it is the cheapest declaration in the language for the same
reason.

### 3.3 The formatter costs nothing per form, and the LSP costs one

Two zeros, measured, and `the_formatter_and_the_lsp_do_not_name_a_form` pins
both.

**`vyrn fmt` names no form at all.** Not one of the 38 appears in `fmt.rs`. That
is RFC-0017's design working exactly as it was written: the formatter reads a
token stream and re-lexes its own output for equality, so it knows about braces
and never about `Stmt::Region`. A form added to the language costs the formatter
nothing unless it adds a TOKEN — which is why `fmt` is a column in §3.3's
keyword table and not in §3.1's.

**The LSP binary names one form, once**: `Expr::Str`, in `rename.rs`, and it is
the string inside an `import`. RFC-0006 made `vyrn-lsp` an adapter
over the frontend's `analyze` and `analyze_linked`, so the per-form editor cost
is in `symbols.rs`, which is the `editor` column above and is 1 for 28 of the 38
rows.

So of the nine passes the brief named, two cost nothing per form, and the census
can say so as a fact rather than as a claim.

### 3.4 The 24 keywords and the 15 contextual words

172 mentions in three files for the keywords. The spellings and the tokens are
read out of the lexer's `keyword_or_ident`, which is the same anchor
`editor/vscode/test/grammar.test.mjs` reads; this census is that map's third
reader.

| keyword | token | lexer | parser | fmt | all three |
|---|---|---|---|---|
| `fn` | `Tok::Fn` | 3 | 20 | 2 | 25 |
| `let` | `Tok::Let` | 3 | 14 | 0 | 17 |
| `mut` | `Tok::Mut` | 3 | 5 | 0 | 8 |
| `if` | `Tok::If` | 3 | 4 | 0 | 7 |
| `else` | `Tok::Else` | 3 | 2 | 0 | 5 |
| `while` | `Tok::While` | 3 | 2 | 0 | 5 |
| `for` | `Tok::For` | 3 | 3 | 0 | 6 |
| `in` | `Tok::In` | 3 | 1 | 0 | 4 |
| `drop` | `Tok::Drop` | 3 | 2 | 0 | 5 |
| `protocol` | `Tok::Protocol` | 3 | 4 | 0 | 7 |
| `import` | `Tok::Import` | 3 | 3 | 0 | 6 |
| `export` | `Tok::Export` | 3 | 2 | 0 | 5 |
| `impl` | `Tok::Impl` | 3 | 3 | 1 | 7 |
| `self` | `Tok::Vself` | 3 | 8 | 1 | 12 |
| `return` | `Tok::Return` | 3 | 2 | 0 | 5 |
| `true` | `Tok::True` | 3 | 1 | 1 | 5 |
| `false` | `Tok::False` | 3 | 1 | 1 | 5 |
| `type` | `Tok::Type` | 3 | 7 | 0 | 10 |
| `where` | `Tok::Where` | 3 | 2 | 0 | 5 |
| `match` | `Tok::Match` | 3 | 1 | 0 | 4 |
| `region` | `Tok::Region` | 3 | 2 | 0 | 5 |
| `spawn` | `Tok::Spawn` | 3 | 1 | 0 | 4 |
| `break` | `Tok::Break` | 3 | 2 | 0 | 5 |
| `continue` | `Tok::Continue` | 3 | 2 | 0 | 5 |

**Every keyword costs the lexer exactly 3**, without exception: the `Tok`
variant, the `keyword_or_ident` arm and the `token_name_and_text` row that
`lex()` reads (RFC-0054). Three copies of one fact, and the count says they have
not drifted. **A keyword is nearly free. The FORM behind it is what costs**, and
§3.1 is where the language's weight is.

45 mentions in four files for the contextual words — the words the lexer hands
back as identifiers and the parser reads by position.

| word | lexer | parser | checker | fmt | all four |
|---|---|---|---|---|---|
| `read` | 0 | 5 | 0 | 0 | 5 |
| `modify` | 0 | 5 | 3 | 0 | 8 |
| `consume` | 0 | 5 | 0 | 0 | 5 |
| `share` | 0 | 1 | 0 | 0 | 1 |
| `gen` | 0 | 3 | 0 | 0 | 3 |
| `test` | 0 | 3 | 0 | 0 | 3 |
| `bench` | 0 | 3 | 0 | 0 | 3 |
| `panic` | 0 | 1 | 2 | 0 | 3 |
| `from` | 0 | 2 | 0 | 0 | 2 |
| `as` | 0 | 2 | 0 | 0 | 2 |
| `extern` | 0 | 3 | 2 | 0 | 5 |
| `lazy` | 0 | 1 | 0 | 0 | 1 |
| `place` | 0 | 1 | 0 | 0 | 1 |
| `logging` | 0 | 2 | 0 | 0 | 2 |
| `contract` | 0 | 1 | 0 | 0 | 1 |

A contextual word costs less than a keyword and buys back a name a user may
bind. `place` is the cheapest kind of survivor: RFC-0120 retired the spelling
and the parser keeps one mention of it, which is the migration refusal RFC-0094
calls a teaching hint. `yield`, its twin in that retired pair, has no row —
nothing in the compiler names it — and §6 is what that costs the editor.

---

## 4. The verdict, one row each

RFC-0126 §4's vocabulary, unchanged. `stays` — the form is irreducible, or a
measurement already refused the collapse. `desugar` — another row already
expresses it and the compiler computes that expansion today. `decide` — a
collapse exists and something has to be chosen first; the row says what.

| form | what it is | RFC | verdict | the desugar, or the reason |
|---|---|---|---|---|
| `Stmt::Let` | `let [mut] n [: T] = e` | RFC-0002 | stays | the only form that introduces a name |
| `Stmt::Assign` | `n = e` | RFC-0002 | decide | one of three stores; a single `Store { place, value }` accepts places the compiler refuses, so §5.5's three parse shapes become three checker rules |
| `Stmt::SetField` | `n.f = e` | RFC-0002 | decide | §5.5, the same row |
| `Stmt::IndexSet` | `n[i] = e` | RFC-0011 | decide | §5.5, the same row |
| `Stmt::Return` | `return [e]` | RFC-0002 | stays | the only way out of a function, and the point every live binding is released at |
| `Stmt::Break` | `break` | RFC-0060 | stays | 32 mentions, 31 of them §3.1.1's floor; nothing else leaves a loop early |
| `Stmt::Continue` | `continue` | RFC-0060 | stays | the floor itself |
| `Stmt::If` | `if c { .. } else { .. }` | RFC-0002 | decide | `Expr::IfExpr` states the branch a second time; §5.4b says which of the two would survive and why that is a value-position decision |
| `Stmt::IfLet` | `if let P = e { .. }` | RFC-0060 | decide | expressible as a parser rewrite onto `match` since RFC-0118 (block arms) and RFC-0121 (`Pattern::Other`) — and §5.4a measured the emitted IR and it is not the same |
| `Stmt::While` | `while c { .. }` | RFC-0002 | stays | the loop `for` and `while let` are both written over |
| `Stmt::ForIn` | `for x in e { .. }` | RFC-0002 | stays | RFC-0089 rule 2's `consuming` flag is an ownership fact with no other carrier |
| `Stmt::Drop` | `drop n` | RFC-0114 | stays | the escape hatch the inference cannot prove; RFC-0118 closed its last hole |
| `Stmt::Expr` | `f(x)` | RFC-0002 | stays | an expression evaluated for its effect |
| `Stmt::Region` | `region { .. }` | RFC-0004 | stays | nothing else bounds an allocation's lifetime to a scope |
| `Expr::Int` | `42` | RFC-0002 | stays | the integer literal |
| `Expr::Byte` | `'c'` | RFC-0057 | decide | `Expr::Int` with a `UInt8` default; the merge adds a width flag to every arm and deletes no rule — RFC-0126 §5.4's shape, and the decision is whether a default type earns a form |
| `Expr::Float` | `1.5` | RFC-0002 | stays | the float literal |
| `Expr::Bool` | `true` | RFC-0002 | stays | the boolean literal |
| `Expr::Str` | `"a"` | RFC-0002 | stays | the string literal, and every interpolation's fragments |
| `Expr::Var` | `x` | RFC-0002 | stays | 202 mentions, the most-named form in the table, and a name has no expansion |
| `Expr::Unary` | `-x`, `!x`, `~x` | RFC-0045 | stays | `-0.0` and `~` have no binary spelling |
| `Expr::Binary` | `a + b` | RFC-0002 | stays | 18 operators in one form: it is already the collapse |
| `Expr::Call` | `f(a)` | RFC-0002 | stays | the form two others would fold into, not one that folds |
| `Expr::Match` | `match e { .. }` | RFC-0005 | stays | the form three desugars target |
| `Expr::IfExpr` | `if c { e } else { e }` | RFC-0030 | decide | `Stmt::If`'s row, one level up |
| `Expr::Try` | `e?` | RFC-0005 | stays | the propagation returns from the enclosing function, and an expression-position `?` has no statement to put a `return` in |
| `Expr::StructLit` | `User { .. }` | RFC-0002 | stays | the record literal |
| `Expr::Field` | `u.name` | RFC-0002 | stays | the projection every place walk reads |
| `Expr::TryConstruct` | `Age?(n)` | RFC-0003 | decide | `Expr::Call` plus one bit; §5.3's rename question |
| `Expr::ArrayLit` | `[a, b]` | RFC-0002 | stays | the literal `Array<T, N>` is the type of |
| `Expr::MapLit` | `["a": 1]` | RFC-0028 | stays | the entries are written, not computed |
| `Expr::Spawn` | `spawn f(a)` | RFC-0025 | decide | `Expr::Call` plus one bit; §5.3's rename question |
| `Expr::Lambda` | `x -> e` | RFC-0110 | stays | defunctionalized (RFC-0037); the literal is where the capture set is read |
| `Expr::Consume` | `consume p` | RFC-0093 | stays | the take, and the third position of the word |
| `Pattern::Variant` | `Some(x)`, `Circle(r)` | RFC-0126 | stays | the one pattern the parser spells, since RFC-0126 §8.10 folded four into it |
| `Pattern::Success` | the tag-1 arm | RFC-0079 | stays | RFC-0126 §8.6 M3 decided it: it names a TAG, and the `??` desugar runs before any type is known |
| `Pattern::Failure` | the tag-0 arm | RFC-0079 | stays | the row above, and the two are one decision |
| `Pattern::Other` | the default arm | RFC-0121 | stays | matches without naming; unspellable in source, and the refutable-`let` desugar has nothing else to end with |
| `imports` | `import { a } from ".."` | RFC-0010 | stays | the loader consumes it and no later pass sees one |
| `type_decls` | `type T = ..` | RFC-0002 | stays | RFC-0126 priced its 33 constructors; the declaration is where they are named |
| `functions` | `fn f(..) -> T { .. }` | RFC-0002 | stays | the unit every engine compiles |
| `protocols` | `protocol P { .. }` | RFC-0002 | stays | a named set of method signatures; RFC-0084 checks conformance against it |
| `contracts` | `contract C { .. }` | RFC-0071 | stays | comptime-only, and zero in both emitters — the cheapest declaration that has a field |
| `impls` | `impl P for T { .. }` | RFC-0086 | stays | where a type's rows come from, and the mechanism RFC-0094 moved six lists into |
| `globals` | top-level `let` | RFC-0013 | decide | `Stmt::Let` plus a module and a doc; decide whether module state is a statement in a synthetic block before initialization order can be one rule |
| `tests` | `test "n" { .. }` | RFC-0015 | decide | its carrier is field for field `benches`'; §8 takes the carrier and the surface keeps both words |
| `benches` | `bench "n" { .. }` | RFC-0055 | decide | the row above, and the two are one decision |

Of the 47 rows: `desugar` 0, `decide` 12, `stays` 35.

**No row says `desugar`, and that is a result.** RFC-0126 found five type
constructors that `types::resolve` already expanded — the compiler was computing
the collapse and keeping the constructor anyway. The form surface has none of
that. Every form is either irreducible or needs a decision first, which means
the surface has no free collapse in it at all, and §5 is the ranking of the
priced ones.

---

## 5. The ranked collapses

Cheapest first. "Saved" is the census total of the rows a collapse deletes.

| § | collapse | saved | moves bytes | changes a diagnostic | changes the grammar | verdict |
|---|---|---|---|---|---|---|
| 5.1 | `tests` and `benches` share one carrier | 0 | no | no | no | **taken, §8** |
| 5.2 | `ArmBody` and `LambdaBody` are one enum | 0 | no | no | no | not taken |
| 5.3 | `Spawn` and `TryConstruct` into `Call` | 0 | no | yes | no | not taken |
| 5.4a | `Stmt::IfLet` into a `match` rewrite | 37 | **yes, measured** | yes | no | not taken |
| 5.4b | `Stmt::If` and `Expr::IfExpr` into one | 38 | yes | yes | no | not taken |
| 5.5 | the three stores into one `Store` | 68 | no | yes | **yes** | not taken |

### 5.1 `tests` and `benches` share one carrier — 0 saved, and every clause of the licence holds

`ast::TestDecl` and `ast::BenchDecl` have the same five fields with the same
types, and `BenchDecl`'s doc comment already says "Structurally identical to
`TestDecl`". Both are checked as Unit-returning function bodies under a
synthetic unspellable name; both are held in a `Vec` field of their own so
`run`/`build`/`emit-ir` never walk them; neither reaches an engine or a shipped
binary.

**It saves no census mention**, and RFC-0126 §8.5 is why that is expected: the
metric counts a NAME, and the two fields keep their names because two
subcommands select them. What it deletes is a struct — one of two statements of
one shape.

It is the only row in the table with a `no` in all three risk columns, so it is
the one §8 takes. §8 also says what it does not do: `test` and `bench` stay two
words, they stay two fields, and `vyrn test` and `vyrn bench` keep every
diagnostic they print.

### 5.2 `ArmBody` and `LambdaBody` are one enum — 0 saved, and the rule does not transfer

Both are `Expr | Block` and `ArmBody`'s own doc calls itself "the twin of
`LambdaBody`". One `Body` enum would state "a body is an expression or a block"
once.

It should not be taken, and RFC-0126 §5.1 gives the reason in the same words: a
collapse must not move a fact out of the type that carries it. The two `Block`
variants do not mean the same thing. A lambda's block returns with `return`, as
a function body does; an arm's block yields nothing at all, and RFC-0118 spent
its whole containment argument on that. Merging the type makes each reader ask
where it is, which is the question the two types answer for free. **Not taken,
and it should not be until one of the two blocks changes meaning.**

### 5.3 `Spawn` and `TryConstruct` into `Call` — 0 saved, a rename

`Expr::Spawn { name, args, line }` and `Expr::TryConstruct { name, args, line }`
have `Expr::Call`'s exact shape. Folding either into `Call` with a flag renames
38 or 36 sites and deletes no rule: every arm that reads the flag is the arm
that read the constructor. RFC-0126 §5.4 refused `Float`/`Float32` for this
reason and `IntN` kept `Int` separate for it.

And it costs a diagnostic. `spawn f(x)` reports "spawn needs an isolated
function"; `Age?(n)` reports a validated type's own refusal. Both messages are
selected today by the form the parser built. **Not taken.**

### 5.4a `Stmt::IfLet` into a parser rewrite onto `match` — 37 saved, and the IR is not the same

The largest saving with no grammar change, and the AST can express it today:
`if let P = e { A } else { B }` is `match e { P => { A } _ => { B } }`, where the
block arms are RFC-0118's and `_` is RFC-0121's `Pattern::Other` — which the
parser can build and a user cannot spell. `while let` is already exactly this
kind of rewrite, onto `if let`, so the machinery and the precedent are both
there.

**The measurement stops it.** The same function, written both ways, through the
textual emitter:

```
;  if let Some(n) = o { return n + 1 } else { return 0 }
  %t3 = icmp eq i64 %t2, 1
  br i1 %t3, label %il.then.0, label %il.else.2

;  match o { Some(n) => { return n + 1 } None => { return 0 } }
  switch i64 %t2, label %me.default.1 [ i64 1, label %me.arm.2 i64 0, label %me.arm.3 ]
```

An `if let` emits a two-way `br i1` on an `icmp`; a `match` emits a `switch i64`
with a default block. The direct backend differs the same way, and the `.wat`
for the two programs is 14,661 and 14,986 bytes. So the rewrite moves every
module in the corpus that writes an `if let`, and RFC-0126 §8.9's rule applies:
a step that moves bytes regenerates the manifest and must be the only step that
does.

**And the two paths are one rule stated twice**, which is the more interesting
half. RFC-0126 §8.11 found exactly this between a built-in sum and a declared
enum — `gen_match_body_boxed` against `gen_match_enum` — and §8.12 (M4a) made
them one emission. `if let` is the same shape one level up: a two-arm match
whose tag test each emitter writes a second time. **Not taken, and the step in
front of it is not this collapse but M4a's twin: one emission for a two-arm
`match` and an `if let`, priced against the manifest.**

### 5.4b `Stmt::If` and `Expr::IfExpr` into one — 38 saved, and the decision is about values

`match` is one node that the checker reads differently in statement and
expression position (RFC-0118's arms-slice address). `if` is two nodes for the
same two positions. One of them could go.

Neither direction is free. `Stmt::If` has block branches and no value;
`Expr::IfExpr` has single-expression branches and must have an `else`. Merging
onto the statement node needs a "the block's value is its last expression" rule
that the language has deliberately never had — RFC-0118 says so and calls it
what keeps block arms from touching the value story. Merging onto the expression
node deletes the statement `if`, which is the most-written form in the corpus.
**Not taken. The decision is what a block's value is, and that is a language
question with no payer.**

### 5.5 the three stores into one `Store` — 68 saved, and the grammar widens

`Stmt::Assign`, `Stmt::SetField` and `Stmt::IndexSet` are `n = e`, `n.f = e` and
`n[i] = e`. One `Store { place: Expr, value: Expr }` would hold all three and
save 68 of their 102 mentions — the largest number in this RFC.

**The refusals are the node's shape.** All three carry `name: String`, not a
place expression, and that is a v1 restriction three passes rely on: the target
is a plain binding, one level deep. `a.b.c = v` and `f().x = v` are refused
because the PARSER cannot build them. A `place: Expr` accepts both, so each
refusal becomes a checker rule, and RFC-0006's accumulation says a parse error
and a checker error are not the same error — the same argument RFC-0126 §5.3
used to refuse `Type::ConstInt`, one level up.

It is also the only row in the table that changes the grammar. The editor
grammar does not describe statements, so nothing there moves; what moves is what
a `.vyrn` file may contain, and the RFC-0017 formatter's re-lex invariant would
have to hold over a shape nothing writes yet. **Not taken. It needs the place
grammar RFC-0082's desugar already assumes, and that is a language decision.**

---

## 6. The defects the census found

### 6.1 Five doc comments describe a surface the code stopped having

RFC-0126 §6 found one of these and RFC-0125 §1.6 predicts them: prose that
retells an RFC does not compile. `ast.rs` carried five, all in the enums this
census reads, and each would have priced a collapse wrong:

- **`enum Stmt`** — "In v0, `if`/`while` are statements (not expressions)".
  RFC-0030 made `if` an expression too, and §5.4b is the row a reader of that
  sentence would not have written.
- **`Expr::Match`** — "Arms are single expressions in v0.1". RFC-0118 gave a
  statement match block arms, and §5.4a's rewrite is possible only because it
  did.
- **`enum Pattern`** — "v0.1 supports the `Option` and `Result` variants".
  RFC-0126 §8.10 folded four constructors into `Pattern::Variant`, and the
  enum's own variants say so.
- **`Expr::Lambda`, twice** — "`|x| expr` or `|x, y| { block }`" is the spelling
  RFC-0110 retired; the parser now refuses `|x|` with a migration hint. And
  "Legal ONLY as a call argument in a function-typed parameter position" is not
  the rule: a `let` with a declared `fn` type takes one, and `std/stream.vyrn`
  writes three of them.

All five now say what the code does, and each cites the RFC that changed it.

### 6.2 The editor grammar colours `yield`, which nothing in the compiler names

`yield` has no row in §3.4 because it earns none: zero in the lexer, zero in the
parser, zero in the checker, zero in the formatter. RFC-0120 retired
`place`/`yield` for the result capability, and the parser kept ONE mention — of
`place`, to report the migration — because a member that begins `place <name>`
deserves a sentence instead of "expected `fn`". Nothing kept `yield`.

`editor/vscode/vyrn.tmLanguage.json` kept a contextual rule for it, so a line
beginning with the word `yield` was coloured as language syntax in every `.vyrn`
file. The rule is deleted. `place` keeps its rule, and the census is what
separates the two: one has a mention and the other has none.

This is the third copy of the keyword surface to be held against the lexer.
`editor/vscode/test/grammar.test.mjs` holds the reserved words;
`compiler/vyrn-cli/tests/contextual_words.rs` holds the playground's contextual
list against the site's; §3.4 now holds both against the compiler's own count.

### 6.3 `contract` is contextual and neither highlighter colours it — recorded, not fixed

RFC-0071 made `contract` a contextual word and §3.4 measures its one mention.
The playground's `CONTEXTUAL` list does not carry it and neither does the editor
grammar, so `contract Wire { .. }` reads as an ordinary identifier in all three
places a reader meets Vyrn.

Not fixed here. A colour is not a refusal: nothing compiles differently, no
diagnostic changes, and adding the word means adding a lookahead to the editor
grammar and a row to two lists that `tests/contextual_words.rs` holds equal.
That is a fourth copy of the same fact, and the RFC that adds it should be the
one that removes the copies. Recorded so the next reader does not have to
measure it again.

---

## 7. What this RFC does not decide

- **The walks.** §3.1.1 measures that about two thirds of every form's cost is
  exhaustive walks that decide nothing, and this RFC does not price them or
  propose to generate them. That is RFC-0125's milestone: the core exists so
  that one walk replaces many, and a census of the walks belongs with the pass
  that deletes them.
- **Eleven of the twelve `decide` rows.** Each names its decision and §5 prices
  five of them. A later slice that takes one has this table to move.
- **The operators.** `BinOp`'s 18 and `UnOp`'s 3 are not forms and are not
  counted; RFC-0045 priced the five it added and nothing has asked since.
- **Whether a form's cost should be its own metric at all.** §3.1 shows 34 of 38
  rows inside one 30-mention band. A metric that does not separate its rows is
  evidence about the compiler's shape, not a ranking of its surface, and this
  RFC uses it as the first.

---

## 8. The collapse taken: `test` and `bench` are one carrier

§5.1 ranked it first among the licensed and §4's two `decide` rows name it.
RFC-0126 §8's M1 set the licence — zero bytes, every engine already agreeing, no
diagnostic worse, one commit — and §5's table has exactly one row with a `no` in
all three risk columns. **Taken, 2026-09-05.**

**What changed.** `ast::BenchDecl` is gone and `ast::TestDecl` is
`ast::NamedBlock`: a named block declaration, which is what `test "n" { .. }`
(RFC-0015) and `bench "n" { .. }` (RFC-0055) both are. `Program::tests` and
`Program::benches` are two `Vec<NamedBlock>` fields under the same two names,
because two subcommands select them and §3.2's census counts the field.

**One production, not two.** The two parser functions were byte-identical apart
from one word inside one diagnostic — `Parser::bench_decl`'s doc comment said
"Structurally identical to `Parser::test_decl`" and it was. `Parser::named_block`
takes the word and both refusals keep the wording they had: "expected a test
name string, found …" and "expected a bench name string, found …". The census
did not predict this one; it is what a reader finds after the carrier merges and
the two functions sit next to each other with nothing between them.

**What did not change, and this is the point.** The surface keeps two words.
`test` and `bench` stay two contextual starters, told apart by the keyword before
the string, and both keep their rows in §3.4. `vyrn test` runs the root module's
`tests` and `vyrn bench` its `benches`. `assert`/`assertEq` stay legal only in a
test and `blackBox` only in a bench, because the checker knows which field it is
walking. Every diagnostic either subcommand prints is untouched: none of them
ever named the struct.

**Why it is zero bytes, and not by luck.** Neither field is walked by `run`,
`build` or `emit-ir`. `Program::tests`'s own doc says why the field exists: a
shipped binary contains no tests, and the string pool and the regex collection
skip both fields by construction. So no emitted module can move, and
`VYRN_WASM_MANIFEST=check` is what says so.

**Priced.** One struct and one function deleted. **35 lines out of the
compiler** — `ast.rs` −7 (24 in, 31 out) and `parser.rs` −28 (17 in, 45 out) —
and the doc comments are most of it, because the two carriers restated each
other's fields and the two productions restated each other's body. Three call
sites outside `ast.rs`: the parser's two callers and the CLI's selected-bench
vector. §3.1, §3.2 and §3.3 do not move at all — not one cell — which §5.1 said
in advance: the metric counts the FIELD and the fields stay. §3.4 moves by two,
in the direction nobody predicted: `test` and `bench` each rise from 2 mentions
to 3, because the merged production takes the word as an ARGUMENT where two
productions each had it only as a position. The step deleted 35 lines and the
census went UP. That is the metric being honest about what it measures — a name
— and it is why the number above is a line count.

**Gates.** In RFC-0125 §1.4's order, one at a time, in the foreground, with
`TMP` and `TEMP` pointed at this worktree's own scratch directory:
`cargo fmt --all --check`, clean; `cargo build --release`; `cargo test -p
vyrn-cli` with no filter, 559 passed and 75 ignored over 76 suites (the census
is 7 of the 559); the `kernel`, `coretables`, `typed` and `effects` suites with
`--ignored` (1, 1, 1 and 2, at 123 s, 106 s, 198 s and 195 s); `fixtures` with
`--ignored`, 70 s; `vyrn-frontend`, 1,249; the workspace less `vyrn-cli` with
`--skip _natively`, 1,425; `vyrn-lsp` from its own directory, 100; `vyrn-genwasm`,
3; `memory` with `--test-threads=1`, 10; parity in release with `--ignored`,
41 of 41 in 254 s; the residue ratchet, 151 s; `VYRN_WASM_MANIFEST=check` on
`wasmhash`, green — **no row moved, which is the zero-byte clause measured**;
the cross-engine generator test with a fresh `VYRN_GEN_CACHE_DIR` and
`--features wasm-gen`, 13, and its corpus test with `--ignored`, green;
`testsweep` with `--ignored`, 103 s; `vyrn doc --std -o ../docs/api --verify`,
41 files up to date; the site — `vyrn run site/export.vyrn out` writes 82 routes
and 14 assets, and `vyrn test` is green over `export.vyrn` (35) and `site/app`
(154, over 24 files; two of the 26 declare no test); the site's 57 node tests;
the playground's 14; and — because §6.2 changed the grammar —
`editor/vscode/test/`, 14 across three files.

**The lesson, and it is the census's own.** A collapse the metric cannot see is
still a collapse, and a metric that cannot see it is still the right metric.
RFC-0126 §8.5 warned that a construction hidden behind a helper stops being
counted; this is the other direction of the same warning. The number that
describes this change is a struct, a function and 35 lines, and the census's own
column of zeros is the honest way to say so.
