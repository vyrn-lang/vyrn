# RFC-0007 — String Templates & Safe Interpolation

- **Status:** **Implemented, §v2 included.** Plain interpolation and tagged
  templates both shipped in Phase 1. **§v2 — the extensible value set — closed by
  RFC-0094 M3:** a type that declares `impl Show for T` is interpolable, in a
  hole and in `x.toString()` alike.
- **Depends on:** RFC-0002 (enums, generics), RFC-0004 (`String` heap values)
- **Enables:** RFC-0008 (Logging), safe `sql`/`latex`/`html` interpolation

> **Implementation status (Phase 1).** Plain interpolation is implemented end to
> end: `"a\{e}b"` inside an ordinary `"..."` string interpolates `e`, with `{`/`}`
> left literal. It **desugars to a `concat`/`str` chain** producing a `String`, so
> the checker/interpreter/both backends need no template-specific logic — and the
> interpreter == native invariant is inherited. `str` was widened from Int-only to
> render **Int, Bool, and String** (each to a fresh owned buffer). The lexer splits
> a string with holes into `Tok::TemplateStr { parts, exprs }` (raw hole sources),
> which the parser re-lexes/parses and folds. A string with no `\{` still lexes to
> a plain `Tok::Str`, so every prior program is unchanged. See
> `examples/interpolation.vyrn`.
>
> **Tagged templates (implemented).** `sql"a\{x}b"` desugars to
> `sql(list(["a","b"]), list([value(x)]))` — the tag is any in-scope function of
> type `(Array<String>, Array<Value>) -> T`. The literal `parts` and the boxed
> `values` reach it as separate arrays, so a value can only ever become a parameter,
> never query structure (the safety property — see `examples/tagged.vyrn`, a `sql`
> tag that renders `$N` placeholders and keeps a `'; DROP TABLE …` value out of the
> text). `Value` is the built-in closed enum `IntVal(Int) | StrVal(String) | BoolVal(Bool)`
> (injected at parse time, matchable by the tag); the `value(x)` builtin boxes a
> scalar into it, and `list(Array<T,N>)` size-erases the fixed literal arrays into
> the growable arrays the tag takes. **A tagged template requires ≥1 interpolation**
> (a hole-less tag is pointless — use a plain string).
>
> **§v2 (implemented — RFC-0094 M3).** The extensible value set is closed. A
> type that declares `impl Show for T` is interpolable: `value(x)` boxes it as
> the `StrVal` its `show` renders to, and `"\{x}"` renders it the same way. The
> `Value` enum stays the closed three, which is deliberate — a tag still reads a
> hole as data and still cannot be handed the user's value itself, so the safety
> property is exactly the one v1 had.

---

## Summary

A string literal may **interpolate** an expression with the `\{ expr }` escape,
and may carry a **tag** prefix that decides how the literal parts and the
interpolated values are assembled:

```vyrn
let msg = "collected \{n} squares, sum = \{total}";   // default tag -> String
let q   = sql"SELECT * FROM users WHERE id = \{id}";   // sql tag -> Query (parameterized)
let doc = latex"\caption{\{title}}";                   // latex tag -> escaped String
```

The design goal is **safe interpolation by construction**: a tagged template
keeps the *literal parts* (compile-time, trusted) separate from the *interpolated
values* (runtime, untrusted and typed), so `sql` can only ever bind a value as a
parameter — never splice it into the query structure. This is the JavaScript
tagged-template / Python t-string (PEP 750) / Scala string-interpolator pattern,
chosen here because "unsafe unless you're careful" is exactly the failure mode
Vyrn rejects everywhere else.

## Why `\{ }` and not `{ }` / `${ }`

The interpolation marker is the **escape** `\{`, placed inside an ordinary
`"..."` string. The consequence that matters: **`{` and `}` remain literal
characters that need no escaping.** The domains interpolation is *for* — LaTeX,
SQL, JSON, regex, C-like code — are made of braces, so a `{}`-based marker would
force doubling/escaping the common case. Here the escape is on the *interpolation*
(the rare thing), and braces cost nothing:

```vyrn
let doc  = latex"\section{\{name}}";        //  \section{…}  braces are literal
let json = "{ \"n\": \{n} }";               //  JSON object braces are literal
```

Other consequences:

- **No new literal type.** Backticks were considered and rejected; `"..."` simply
  gains one new escape. A string containing no `\{` is byte-for-byte unchanged, so
  **every existing program and example still lexes identically** — this is a purely
  additive change.
- To write a literal backslash-then-brace, escape the backslash: `"\\{"`.
- `\{` with no closing `}` (or an ill-formed inner expression) is a **lex/parse
  error**, not a silently-literal `{`.

## Anatomy & desugaring

A template is *n+1* literal fragments interleaved with *n* interpolated
expressions. It desugars to a call to its tag with two arguments — the fragments
and the values:

```
sql"SELECT * FROM users WHERE id = \{id} AND org = \{orgId}"
        ↓
sql( parts:  ["SELECT * FROM users WHERE id = ", " AND org = ", ""],
     values: [ box(id), box(orgId) ] )
```

- `parts` always has exactly `values.length + 1` fragments (empty strings at the
  ends when a template starts/ends with an interpolation).
- `parts` is a compile-time-constant `Array<String>` — **its contents can never
  come from a value.** This is the whole safety argument.
- Each interpolated value is **boxed by its static type** into the interpolation
  value type (below), so the tag receives typed values, not pre-stringified text.

An **untagged** template `"… \{x} …"` desugars to the built-in default tag, which
renders each value and concatenates, producing a `String`. So plain interpolation
is just the identity case of the general mechanism.

## The interpolation value type

To carry heterogeneous values (`\{intId}` and `\{name}` in one template) a tag
needs a common value type. Two stages:

### v1 — a closed, built-in set (implementable on today's type system)

```vyrn
type Value = | IntVal(Int) | StrVal(String) | BoolVal(Bool)
```

The compiler boxes each `\{ e }` into the variant matching `e`'s static type. A
tag is then an ordinary function:

```vyrn
fn latex(parts: Array<String>, values: Array<Value>) -> String { … }
```

This needs **no new type-system machinery** — enums + arrays already exist
(RFC-0002). It covers the scalar cases SQL/LaTeX care about. Limitation: only the
built-in scalars are interpolable.

### v2 — open/extensible (after protocols) — **landed, RFC-0094 M3**

Any type implementing a rendering protocol becomes interpolable. The protocol is
`Show`, with one method `fn show(self) -> String`.

The shape this section predicted is **not quite the shape that landed**, and the
difference is worth recording. It expected `Value` to be "replaced by a protocol
bound" — a wider element type. What landed keeps the element type exactly as it
was: a hole of a declared type reaches the tag as the `StrVal` its `show`
rendered to. Two reasons, and both are properties this RFC already argued for.

1. **The safety property is about what a tag can be handed.** A tag receives
   data, never structure. A `StrVal` is data. Widening the array to hold the
   user's own value would give a tag something to reach into.
2. **A wider element type has nowhere to live.** Vyrn monomorphizes and has no
   function pointers (RFC-0037), so `Array<Value>` cannot hold one element per
   declared type. Rendering at the box is what a language without existentials
   can do, and it is enough for what §v2 asked for.

So v1 was forward-compatible in the way that mattered: the `parts/values` shape
did not change at all.

## Tags

A tag is a plain identifier prefix on the string. Resolution:

- **Untagged** → the built-in default tag → `String`.
- **`name"…"`** → calls `name(parts, values)`; the template's type is that
  function's return type. `sql` → `Query`, `latex`/`html` → `String` (escaped),
  `json` → `Json`, etc.

A tag is just a function with the signature `(Array<String>, Array<Value>) -> T`.
Nothing about tags is special-cased beyond the desugaring; they are ordinary
library code, which is what makes the feature open-ended.

### Worked example — SQL safety by construction

```vyrn
let userName = "'; DROP TABLE users; --";
let q = sql"SELECT * FROM users WHERE name = \{userName}";
//  q.text   == "SELECT * FROM users WHERE name = $1"
//  q.params == [ StrVal(userName) ]
```

Because `parts` is literal and `userName` can only land in `values`, `sql` places
it in a bound parameter (`$1`) — the malicious payload is data, never SQL. There
is no code path by which a value becomes query structure; the safety is a property
of the desugaring, not of the programmer remembering to escape.

### Worked example — LaTeX / escaping tags

```vyrn
let title = "Cost: 50% of R&D_budget";
let doc = latex"\caption{\{title}}";
//  escapes % _ & # { } in the *value* -> "\caption{Cost: 50\% of R\&D\_budget}"
//  the literal \caption{…} is untouched
```

## Interaction with logging (RFC-0008)

Logging **consumes** templates rather than inventing placeholders:

```vyrn
log.info("collected \{n} squares, sum = \{total}");
```

`log.info` receives the template. A trivial logger renders it to a line; a
**structured** logger keeps `parts` + `values` as message-plus-fields for free
(`{ msg: "collected {} squares…", fields: [n, total] }`) — so structured logging
falls out of the same mechanism instead of needing its own format language.

## Implementation notes

- **Lexing.** An interpolated string is no longer a single token. The lexer splits
  `"… \{ e } …"` into a sequence — `TemplateStart(fragment)`,
  then for each hole the embedded expression tokens and a `TemplateMid(fragment)`,
  ending with `TemplateEnd(fragment)` — or emits a structured template token the
  parser expands. Fragment text still has its ordinary escapes (`\n`, `\"`, …)
  decoded; `\{` is the one escape that opens a hole and `\\{` is a literal `\{`.
- **Parsing.** A template becomes an AST node `Expr::Template { tag: Option<String>,
  parts: Vec<String>, values: Vec<Expr> }`.
- **Checker.** Each `value` is checked, then boxed into `Value` by its static type
  (reusing the enum-construction path). An untagged template has type `String`; a
  tagged one has the tag function's return type, and the tag must have signature
  `(Array<String>, Array<Value>) -> T`.
- **Lowering.** Desugar to the tag call over a fixed `Array<String>` (a fixed
  `ArrayN` of string globals) and an `Array<Value>` of boxed values. Both backends
  and the interpreter already lower enums, arrays, and calls — so no new runtime
  primitive is required for v1; the default tag is `str`+`concat` under the hood.
- **Parity.** Interpreter and native must agree, as always; because the desugaring
  is to existing constructs, this is inherited.

## Open questions

- **Q1 — multi-line templates.** Should interpolated `"..."` allow newlines
  (helpful for SQL/LaTeX blocks), or keep strings single-line and add a separate
  block-string form later? *(Leaning: allow raw newlines inside `"..."`.)*
- **Q2 — default rendering of `Value`.** Exact `Bool`/`Int`/`String` rendering for
  the default tag (e.g. `true`/`false`, decimal, verbatim) — trivial but must be
  pinned so interpreter == native.
- **Q3 — tag namespacing.** Is a tag any in-scope function of the right signature,
  or a distinguished declaration (`tag fn sql(...)`)? *(Leaning: any function with
  the signature; no new keyword.)*
- **Q4 — nesting.** Interpolations containing templates (`"\{ cond ? a"…" }"`).
  Falls out of "the hole is an ordinary expression," but worth a test.
- **Q5 — extensible `Value`.** **Answered by RFC-0094 M3.** The protocol is
  `Show`; the box stays the closed three and a declared type renders into
  `StrVal`. v1's shape did not have to change.
