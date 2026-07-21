# RFC-0057 — Byte Literals: `'{'` as `UInt8`

- **Status:** Implemented
- **Depends on:** RFC-0022 (`s[i]: UInt8` — byte indexing made bytes the
  working unit of every scanner), RFC-0046 (`std/strings`), RFC-0054
  (`std/scan` — the scanners this de-noises)
- **Evidence (user):** "Doesn't it look messy?" — a survey of `std/` found
  the magic-number wall everywhere: 59 bare byte comparisons in `vyx.vyrn`,
  46 in `i18n.vyrn`, 31 in `tw.vyrn` (`if b == 123`, `b >= 97 && b <= 122`).
  There is currently **no way to write `'{'`** in Vyrn, so every scanner
  spells it `123`.

---

## Surface

```vyrn
if b == '{' {            // today: if b == 123 {
    depth = depth + 1
}
let isDigit = b >= '0' && b <= '9'
```

- **`'c'` is a `UInt8` literal.** Single quotes, exactly one byte of
  content. NOT a new type — it lexes to the same integer literal `123`
  would; the checker sees a `UInt8` constant (coercible exactly as integer
  literals are today, so `let x: Int64 = '{'` follows the existing
  literal-coercion rules).
- Content: one printable ASCII char (`0x20..0x7E`, excluding `'` and `\`),
  or an escape: `'\n'` `'\t'` `'\r'` `'\''` `'\\'` `'\0'` `'\xNN'` (two hex
  digits). A multi-byte (non-ASCII) character inside `'…'` is a lexer error
  (`byte literal must be a single ASCII byte; write the UTF-8 bytes
  explicitly`) — this is a BYTE literal, not a char type, and it does not
  pretend otherwise.
- Zero backend work by construction: after lexing it IS an integer literal.
  Interp/native/wasm are untouched; parity cannot move.

## Non-goals (locked out)

No `Char`/`Rune` type, no multi-byte scalar literals, no `'…'` strings
(single-quoted strings stay illegal — the error message for `'ab'` must
say so plainly), no byte-string literals (`b"…"`).

## Tooling

- **fmt**: preserves the literal exactly as written (a `'{'` never rewrites
  to `123` or vice versa); safety invariant (re-lex equality) holds.
- **Editor**: grammar scope `constant.character` for `'…'`; no snippet.
- **LSP**: hover on a byte literal shows `UInt8` (whatever the existing
  literal-hover path shows for `123`).
- Lexer conflict check: `'` is currently unlexable, so this is a pure
  addition; the RFC-0007 template lexer and `vyrn"…"` quotes (RFC-0054)
  must still lex `'` INSIDE string/template bodies as plain text.

## Verification

1. Lexer tests: every escape, `'\xff'`, error cases (`''`, `'ab'`, `'é'`,
   unterminated) with pinned wording.
2. Coercion: `'{' == bytes("{")[0]`, `let x: Int64 = 'a'` behaves as the
   integer literal `97` does.
3. fmt idempotence + re-lex equality on a file full of byte literals.
4. A parity example exercising byte literals in scanner-shaped code.
5. Full suite + LSP + parity green; 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy (grammar changed ⇒ extension files too).

Adoption across `std/` is NOT this RFC — it lands with the RFC-0059 sweep
so the mechanical churn happens once.

---

## As landed

Byte literals ship exactly as specified: `'c'` is one ASCII byte, `UInt8` by
default, coercible like an integer literal.

**Where it moved.** A distinct `Tok::Byte(u8)` (lexer) and `Expr::Byte(u8)`
(AST) carry the byte value. The lexer's byte-literal scan is a single shared
helper `lex_byte_literal` used by BOTH `lex` and `lex_with_trivia`, so their
token boundaries agree (the fmt safety invariant). The checker types `Expr::Byte`
as `UInt8` when unconstrained and coerces it to an expected/sibling integer type
exactly as `Expr::Int` does; every other pass (interp, `vyrn-codegen`, consteval,
own/move/loader analyses, symbols hover/completion) treats `Expr::Byte(n)`
identically to `Expr::Int(n)`.

**Deviation from "lexes to the same integer literal 123 would" / "zero backend
work".** A separate `Expr::Byte` variant was introduced rather than reusing
`Expr::Int`, because the locked text also requires "the checker sees a `UInt8`
constant" and UInt8 hover — the byte-ness must survive to the checker, and there
is no side channel on `Expr::Int`. This is the closest sound thing: the variant
is a *checker/tooling* distinction only. Every backend forwards it to the
identical integer codegen (`Expr::Byte(n)` → the same operand as `Expr::Int(n)`),
so there is no new runtime representation and no IR change — **parity output is
byte-identical and cannot move** (verified three-way). The interp/codegen arms
are one-line forwards, not semantic work.

**Escapes and errors** are precisely the locked set: `\n \t \r \' \ \0 \xNN`,
printable ASCII `0x20..0x7E` otherwise. `\u{…}` (a code-point escape) is gone —
this is a byte, not a `Char`. Pinned wording: `''` → "empty byte literal …";
`'ab'` → "single-quoted strings are not allowed: '…' is a single byte …"; `'é'`
→ "byte literal must be a single ASCII byte; write the UTF-8 bytes explicitly";
unterminated → "unterminated byte literal"; `'\u{41}'` → "unknown byte escape
`\u`".

**Pre-existing char lexer replaced.** A prior lexer path produced a *code-point*
`Tok::Int` for `'…'` (never reachable in the surface language as documented, but
used by `examples/encoding.vyrn`). It is replaced by byte semantics. `encoding.vyrn`
migrated `'\u{e9}'` → `'\xe9'` (byte `0xE9` equals é's code point 233, so
`chars(s)[3] == '\xe9'` still prints `true`); one interpreter test that asserted
the old code-point behaviour was rewritten to byte-literal semantics. A byte
literal also adapts to an integer sibling in a binary op (like an int literal),
so `chars(s)[i] == '\xe9'` (Int64 vs byte) type-checks.

**Adoption** across `std/` (replacing bare `b == 123` walls) is deliberately NOT
done here — it is the RFC-0059 sweep. Only the forced `encoding.vyrn` fix and a
new parity example (`examples/bytecount.vyrn`, byte literals in scanner-shaped
code) land now.

**Tooling.** fmt preserves the literal verbatim (raw-text token stream;
re-lex-equality holds, idempotent — corpus_fmt green). Grammar: a
`constant.character.vyrn` rule (with the escape highlighted) in
`editor/vscode/vyrn.tmLanguage.json`, placed after `#strings` so a `'` inside a
string stays plain text. LSP hover on a byte literal shows `UInt8`.

**Verification.** New lexer tests (every escape, `'\xff'`, and the pinned error
cases `''`/`'ab'`/`'é'`/unterminated/`'\u{…}'`), plus a test that a `'` inside a
string/triple-quote/template body stays plain text. Coercion tests: `'{' ==
bytes("{")[0]` and `let x: Int64 = 'a'` behave as `97`. Full workspace suite
**989** passing; `vyrn-lsp` **40** passing; three-way interp==native==wasm parity
green; 0 new clippy warnings.

**LSP redeploy.** `editor/vscode/server/vyrn-lsp.exe` rebuilt (release) and
hash-verified equal to the build output:
`349340C826D71BE9631F5623E7D15CF2102C6A3DD608BEEB8A8DF8AD3E562633`.
