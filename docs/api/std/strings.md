# std/strings

std/strings — string helpers, written in Vyrn itself on the `slice` builtin
(RFC-0046) and `bytes()`. A `String` is UTF-8 bytes; indices and lengths are
BYTE offsets, and `slice` keeps every result on a codepoint boundary (a cut
inside a multi-byte character traps). The case and whitespace helpers are
ASCII-only by necessity — a real Unicode case fold or whitespace class would
need tables this library does not carry; non-ASCII bytes pass through
unchanged, so multi-byte text is never corrupted (documented per function).

The three predicates `s.contains(sub)` / `s.startsWith(sub)` /
`s.endsWith(sub)` are compiler BUILTINS (methods, also callable as free
functions) available everywhere WITHOUT importing this module — so a single
`import { .. } from "std/strings"` plus those builtins covers the surface.
`indexOf`/`lastIndexOf` return an honest `Option<Int64>` (the house idiom —
no `-1` sentinel), `None` when the needle is absent.

## fromBytesOr

```vyrn
fn fromBytesOr(b: Array<UInt8>, fallback: String) -> String
```

`stringFromBytes(b)` where the bytes are KNOWN to be valid UTF-8 — an ASCII
transform of, or an ASCII-boundary slice/rebuild of, already-valid text — so
the `Err` arm is provably unreachable and `fallback` is returned only to make
the total function total. This is the ONE shared home for that pattern: the
generators (`tw`, `i18n`, `vyx`, `ui`, `graphql`) used to scatter six copies of
`match stringFromBytes(..) { Ok(v) => v, Err(_) => "" }`, which hid whether the
failure was truly impossible or a swallowed real error. Call this ONLY when the
bytes cannot be invalid UTF-8; decode a genuinely fallible byte source with
`stringFromBytes` and handle the `Err`.

## repeat

```vyrn
fn repeat(s: String, n: Int64) -> String
```

`s` repeated `n` times ("" for n <= 0).

## joinWith

```vyrn
fn joinWith(parts: Array<String>, sep: String) -> String
```

The elements of `parts` joined with `sep` between them.

## substring

```vyrn
fn substring(s: String, start: Int64, end: Int64) -> String
```

A byte-range substring — a friendly alias of the `slice` builtin. `start`/`end`
are byte offsets; a cut inside a multi-byte UTF-8 character or an out-of-range
offset traps (RFC-0046).

## indexOf

```vyrn
fn indexOf(s: String, needle: String) -> Option<Int64>
```

The byte offset of the first occurrence of `needle` in `s`, or `None`. An
empty needle matches at 0. Byte-level search (needle bytes compared against
`s`'s), so it is UTF-8-safe: a match only ever lands on a codepoint boundary
because `needle` is itself valid UTF-8.

## lastIndexOf

```vyrn
fn lastIndexOf(s: String, needle: String) -> Option<Int64>
```

The byte offset of the LAST occurrence of `needle` in `s`, or `None`. An empty
needle matches at `s.length`.

## split

```vyrn
fn split(s: String, sep: String) -> Array<String>
```

Split `s` on every occurrence of `sep`. Adjacent/leading/trailing separators
yield empty segments (like Rust/JS). An EMPTY separator returns `[s]` unsplit:
per-byte "char" splitting would cut multi-byte UTF-8 characters (a `slice`
trap), so it is deliberately not done — iterate codepoints another way if you
need them.

## lines

```vyrn
fn lines(s: String) -> Array<String>
```

Split `s` into lines on `\n`, stripping a trailing `\r` from each (so a
`\r\n`-delimited file reads exactly like `readLine()`). A trailing newline
does NOT produce a final empty line; a non-empty tail without a newline is
its own line.

## splitWhitespace

```vyrn
fn splitWhitespace(s: String) -> Array<String>
```

Split `s` on runs of ASCII whitespace, dropping empty segments (leading,
trailing, and repeated whitespace produce no empty elements).

## trimStart

```vyrn
fn trimStart(s: String) -> String
```

`s` with leading ASCII whitespace removed.

## trimEnd

```vyrn
fn trimEnd(s: String) -> String
```

`s` with trailing ASCII whitespace removed.

## trim

```vyrn
fn trim(s: String) -> String
```

`s` with leading AND trailing ASCII whitespace removed.

## toLower

```vyrn
fn toLower(s: String) -> String
```

`s` lowercased over ASCII `A`–`Z`. Non-ASCII bytes pass through unchanged (a
real Unicode case fold is out of scope).

## toUpper

```vyrn
fn toUpper(s: String) -> String
```

`s` uppercased over ASCII `a`–`z`. Non-ASCII bytes pass through unchanged.

## replace

```vyrn
fn replace(s: String, from: String, to: String) -> String
```

Every non-overlapping occurrence of `from` in `s` replaced with `to`. An empty
`from` returns `s` unchanged.

## padStart

```vyrn
fn padStart(s: String, len: Int64, fill: String) -> String
```

`s` left-padded with `fill` to at least `len` BYTES. `fill` should be ASCII
(typically a single character such as `" "` or `"0"`); a multi-byte `fill`
that does not tile the padding width evenly traps on a codepoint boundary.
A `s` already `len` bytes or longer is returned unchanged.

## padEnd

```vyrn
fn padEnd(s: String, len: Int64, fill: String) -> String
```

`s` right-padded with `fill` to at least `len` BYTES (see `padStart` for the
ASCII `fill` note).

## toHex

```vyrn
fn toHex(n: UInt64) -> String
```

The lowercase 16-digit hex of a `UInt64` (e.g. a hash fingerprint). Integer
`.toString()` is decimal-only, so hex is built nibble-by-nibble via the
bitwise ops (RFC-0045).
