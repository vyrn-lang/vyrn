# std/scan

std/scan: one comment- and string-aware cursor over foreign text (CSS, ICU
messages, HTML templates, SDL) for generators.

Offsets are byte offsets. The scanner is a record advanced through `modify`
functions, which keep `line`/`col` in sync. `until`, `untilStr` and
`balanced` skip over quoted strings and line and block comments, so a
delimiter inside one never ends the scan. Block comments do not nest: the
first `blockClose` closes one, as in CSS and C.

## Scanner

```vyrn
type Scanner = { src: String, pos: Int64, line: Int64, col: Int64, lineComment: String, blockOpen: String, blockClose: String, quote1: Int64, quote2: Int64, escape: Int64 }
```

A cursor over `src`, with 1-based `line`/`col` and its lexical config.

## newScanner

```vyrn
fn newScanner(src: String) -> Scanner
```

A Vyrn-flavored scanner: `//` line comments, `"`/`'` strings, `\` escape. Vyrn
has no block comments, so `blockOpen`/`blockClose` are disabled.

## cssScanner

```vyrn
fn cssScanner(src: String) -> Scanner
```

A CSS-flavored scanner: no line comments, `/* */` block comments, `"`/`'`
strings, `\` escape.

## scanner

```vyrn
fn scanner(src: String, lineComment: String, blockOpen: String, blockClose: String, quote1: Int64, quote2: Int64, escape: Int64) -> Scanner
```

A fully configured scanner. Pass `""` for `lineComment`/`blockOpen` to disable
that comment kind and `-1` for any disabled byte.

## atEnd

```vyrn
fn atEnd(sc: Scanner) -> Bool
```

## peek

```vyrn
fn peek(sc: Scanner) -> Int64
```

The current byte, or `-1` at end of input.

## peekAt

```vyrn
fn peekAt(sc: Scanner, n: Int64) -> Int64
```

The byte `n` ahead, or `-1` past the end.

## looksAt

```vyrn
fn looksAt(sc: Scanner, s: String) -> Bool
```

Whether the input at the cursor begins with `s`.

## advance

```vyrn
fn advance(sc: modify Scanner) -> Unit
```

Advances one byte, keeping `line`/`col` in sync.

## skipWs

```vyrn
fn skipWs(sc: modify Scanner) -> Unit
```

Skips ASCII whitespace and comments.

## ident

```vyrn
fn ident(sc: modify Scanner) -> String
```

Consumes a `[A-Za-z0-9_]+` run and returns it (empty if none here).

## quotedString

```vyrn
fn quotedString(sc: modify Scanner) -> Option<String>
```

If the cursor is at a quote, consumes the quoted run and returns its inner
text with escapes as written; the caller decodes its own dialect. `None` if
not at a quote. An unterminated string returns the text to end of input.

## until

```vyrn
fn until(sc: modify Scanner, stop: Int64) -> String
```

Consumes up to, not including, the first top-level byte `stop`, skipping
quoted strings and comments, and returns the consumed text. Stops at end of
input if `stop` never appears at the top level.

## untilStr

```vyrn
fn untilStr(sc: modify Scanner, stop: String) -> String
```

Like `until`, but for the multi-byte string `stop`. The cursor is left on
`stop`.

## balanced

```vyrn
fn balanced(sc: modify Scanner, open: Int64, close: Int64) -> String
```

With the cursor at byte `open`, consumes the balanced region through the
matching `close` (nesting, string and comment aware) and returns the text
between them. An unbalanced region returns everything to end of input. Not
at `open`: returns `""` and consumes nothing.
