# RFC-0017 — `velac fmt`: the Canonical Formatter

- **Status:** Implemented — `vela_frontend::fmt`, `velac fmt [--check]`, LSP
  `textDocument/formatting`; the whole corpus is canonical and re-verified by
  the three-way parity harness.
- **Depends on:** RFC-0006 (diagnostics/lexer positions), the no-semicolon
  and lowerCamelCase surface decisions

> **Motivation.** Every serious language ships one formatter with one style
> and no debates. Vela's style already exists in practice (45 examples +
> std/ are hand-consistent); this RFC writes it down and makes `velac fmt`
> enforce it — plus LSP `textDocument/formatting`, so format-on-save works
> in VS Code.

---

## The canonical style (normative)

- **Indentation:** 4 spaces, driven by brace depth. No tabs.
- **Semicolons:** none — a trailing `;` is legal to *parse* (legacy) and is
  **removed** by the formatter.
- **Spacing:** one space around binary operators and `=`/`->`/`=>`; one
  space after `,` and `:`, none before; no space inside `(..)`/`[..]`; one
  space before an opening `{`; no trailing whitespace; file ends with
  exactly one newline.
- **Blank lines:** at most one consecutive blank line anywhere; a `///` doc
  block sits directly above its declaration (no blank in between — the
  parser's attachment rule made observable).
- **Comments:** preserved verbatim; an own-line comment keeps its own line
  (indented to context); a trailing comment gets exactly one space before
  `//`.
- **Line structure is the author's:** v1 **never joins or splits lines** —
  no width-driven reflow. It normalizes indentation and intra-line spacing
  only. (Reflow is a possible v2; predictability first.)

## The safety invariant (what makes this trustworthy)

Formatting must be **meaning-preserving and idempotent**, mechanically
checked:

1. `lex(fmt(src))` equals `lex(src)` as a token sequence, modulo removed
   `Semi` tokens — the formatter cannot change what the parser sees.
2. `fmt(fmt(src)) == fmt(src)` byte-for-byte.

Both are enforced by tests over the whole corpus (examples/ + std/), and
`fmt` itself refuses to write output that violates (1) — it prints an error
and leaves the file untouched (a formatter bug must never corrupt source).

## Implementation shape

- A **comment-preserving lex pass**: the existing lexer drops `//` comments
  (keeping only `///` docs). Add an additive mode (new entry point; the
  existing `lex()` is untouched) that also emits comment tokens with
  positions.
- `vela_frontend::fmt(source) -> Result<String, Diagnostic>` — a printer
  over that token stream: indent from brace/bracket depth, the spacing
  table above, semicolon dropping, blank-line collapsing. No AST needed
  (nothing moves across lines), which is what makes invariant (1) cheap to
  guarantee. A source that fails to lex returns its lex error (fmt requires
  lexable input; it does NOT require parseable input — formatting a file
  with a parse error still works, which matters for format-on-save).
- **CLI:** `velac fmt <file>...` formats in place; `--check` writes nothing,
  lists files that would change, exits 1 if any. Manifest-aware `velac fmt`
  with no args formats the project main + its local (non-remote) imports.
- **LSP:** advertise `document_formatting_provider`; the handler runs the
  same `fmt` on the cached document and returns one whole-document edit.
  VS Code format-on-save then works with zero extension changes.
- **Corpus:** the landing commit runs `velac fmt` over examples/ and std/ —
  the diff should be near-empty (the style *is* the corpus style); whatever
  it does change is reviewed as a style-rule bug or a corpus inconsistency,
  then the parity harness re-verifies everything.

## Implementation decisions (v1)

The printer works over a comment-preserving token stream
(`lexer::lex_with_trivia`) and only ever chooses the whitespace *between* raw
token texts — it never re-synthesizes a token's spelling, so string/char/number
literals (and `\{ }` interpolation holes) are reproduced byte-for-byte. A few
rules that the normative style left implicit had to be pinned down:

- **Generic `<`/`>` vs comparison.** Disambiguated by *source tightness*: a
  generic bracket is written tight against its neighbours (`Box<T>`,
  `Array<Int64>`, `Array<String, 3>` — the const-generic size counts, so a `>`
  after an integer is still generic), whereas a comparison is spaced (`a < b`,
  `i < 1`). This matches the entire corpus and needs no type information (fmt
  builds no AST). Space *after* a generic `>` follows the ordinary rules, so
  `Box<T> =` keeps its space and never fuses into `>=`; a generic `>` before `(`
  attaches (`fn id<T>(x)`).
- **Unary vs binary `-`.** By the previous token: `-` is unary unless it follows
  an operand (identifier, literal, `)`, `]`, `?`). Unary binds tight (`-1`,
  `(-1)`, `=> -1`); binary is spaced (`a - 1`).
- **Leading-pipe enums.** A line whose first token is `|` indents one level
  deeper than brace depth (`type Shape =` then `    | Circle(Int64)`), the one
  construct whose indentation is not brace-driven.
- **Tagged templates.** A string literal written tight against a preceding
  identifier stays tight (`sql"..\{}.."`), matching RFC-0007. The parser keys on
  same-line adjacency (not column), so this is cosmetic — but tight is the
  canonical form. `return "x"` / `from "path"` have a source space and are
  unaffected.
- **`///` doc / blank-line interaction.** A blank line between a `///` block and
  a declaration is **load-bearing** — the parser *detaches* the doc on a gap
  (a file-header block belongs to the file, observable via hover and
  `schemaOf(T).doc`). The formatter therefore does **not** remove it; it only
  collapses 2+ blanks to one. An already-adjacent doc stays adjacent. (This is
  the meaning-preserving reading of "a `///` doc block sits directly above its
  declaration": the token stream is identical either way, but attachment is not,
  so line structure is preserved rather than rewritten.)
- **Semicolons.** Dropped. A stray single-line `a; b` becomes `a b`; v1 does not
  reflow it onto two lines, and the no-semicolon parser accepts the result.

## Out of scope

Width-driven reflow (line breaking/joining), import sorting, configurable
style (there is exactly one style, deliberately), organizing/removing dead
code, editor range-formatting (whole-document only in v1).
