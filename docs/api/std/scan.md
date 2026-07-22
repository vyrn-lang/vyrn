# std/scan

std/scan (RFC-0054) — one shared, comment- and string-aware cursor over
foreign text (CSS, ICU messages, HTML templates, SDL). Every generator that
walks text used to hand-roll this and got it wrong: a delimiter inside a
string or comment was miscounted (the `std/vyx` scanner bugs), an apostrophe
inside an ICU message swallowed a parameter. Written once, tested once.

A `String` is UTF-8 bytes; all offsets are BYTE offsets (like `std/strings`).
The scanner is a plain record advanced through `modify` cursor functions, so
`line`/`col` stay in sync. Comment markers, quote bytes, and the escape byte
are configurable; `-1` disables a byte-valued setting and `""` a line- or
block-comment marker. The quote/comment awareness is the whole point: `until`,
`untilStr`, and `balanced` skip OVER quoted strings, line comments, AND
`/* */`-style block comments, so a delimiter hiding inside one never ends the
scan early. Block comments are non-nesting (the CSS/C rule: the first
`blockClose` after a `blockOpen` closes it), which is exactly what CSS needs.

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
strings, `\` escape. The shared cursor for the `std/tw` CSS handling.

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

Whether the cursor is at end of input.

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
fn advance(sc: Scanner) -> Unit
```

Advance one byte, keeping `line`/`col` in sync (an LF starts a new line).

## skipWs

```vyrn
fn skipWs(sc: Scanner) -> Unit
```

Skip ASCII whitespace AND comments (line comments through end of line, block
comments through their close). Comment-aware so `until`/`balanced` never
mistake comment text for structure.

## ident

```vyrn
fn ident(sc: Scanner) -> String
```

Consume a `[A-Za-z0-9_]+` run and return it (empty string if none here).

## quotedString

```vyrn
fn quotedString(sc: Scanner) -> Option<String>
```

If the cursor is at a quote, consume the whole quoted run (respecting the
escape byte) and return its INNER text (quotes stripped, escapes left as
written — the caller decodes per its own dialect). `None` if not at a quote.

## until

```vyrn
fn until(sc: Scanner, stop: Int64) -> String
```

Consume up to (but NOT including) the first top-level occurrence of byte
`stop`, skipping over quoted strings and comments, and return the consumed
text. Stops at end of input if `stop` never appears at the top level.

## untilStr

```vyrn
fn untilStr(sc: Scanner, stop: String) -> String
```

Like `until`, but stops at the first top-level occurrence of the multi-byte
string `stop` (string/comment aware). The cursor is left ON `stop`.

## balanced

```vyrn
fn balanced(sc: Scanner, open: Int64, close: Int64) -> String
```

Assuming the cursor is at byte `open`, consume the balanced region through the
matching `close` (nesting-aware, string/comment aware) and return the INNER
text (between the outermost `open`/`close`). If `open`/`close` never balances,
returns everything to end of input. A no-op (empty string) if not at `open`.
