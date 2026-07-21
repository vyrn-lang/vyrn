# RFC-0057 — Byte Literals: `'{'` as `UInt8`

- **Status:** Locked design
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
