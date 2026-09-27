# std/strings

## fromBytesOr

```vyrn
fn fromBytesOr(b: Array<UInt8>, fallback: String) -> String
```

`stringFromBytes(b)` for bytes known to be valid UTF-8, such as an ASCII
transform of valid text. `fallback` exists only to keep the function total.
Decode a fallible byte source with `stringFromBytes` and handle the `Err`.

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

A byte-range substring. An out-of-range offset or a cut inside a multi-byte
character panics, naming the offset. The one place in `std/` that turns a
`SliceError` into a crash; a caller that can receive a bad range calls
`slice` and matches.

## indexOf

```vyrn
fn indexOf(s: String, needle: String) -> Option<Int64>
```

The byte offset of the first occurrence of `needle` in `s`, or `None`. An
empty needle matches at 0.

## lastIndexOf

```vyrn
fn lastIndexOf(s: String, needle: String) -> Option<Int64>
```

The byte offset of the last occurrence of `needle` in `s`, or `None`. An
empty needle matches at `s.byteLength`.

## split

```vyrn
fn split(s: String, sep: String) -> Array<String>
```

Splits `s` on every occurrence of `sep`. Adjacent, leading and trailing
separators yield empty segments. An empty `sep` returns `[s]` unsplit, because
per-byte splitting would cut multi-byte characters.

## lines

```vyrn
fn lines(s: String) -> Array<String>
```

Splits `s` into lines on `\n`, stripping a trailing `\r` from each, as
`readLine()` does. A trailing newline makes no final empty line.

## splitWhitespace

```vyrn
fn splitWhitespace(s: String) -> Array<String>
```

Splits `s` on runs of ASCII whitespace, with no empty segments.

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

`s` with leading and trailing ASCII whitespace removed.

## toLower

```vyrn
fn toLower(s: String) -> String
```

`s` lowercased over ASCII `A`-`Z`.

## toUpper

```vyrn
fn toUpper(s: String) -> String
```

`s` uppercased over ASCII `a`-`z`.

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

`s` left-padded with `fill` to at least `len` bytes. Panics when a multi-byte
`fill` does not tile the padding width.

## padEnd

```vyrn
fn padEnd(s: String, len: Int64, fill: String) -> String
```

`s` right-padded with `fill` to at least `len` bytes. Panics when a
multi-byte `fill` does not tile the padding width.

## toHex

```vyrn
fn toHex(n: UInt64) -> String
```

The lowercase 16-digit hex of `n`.

## editDistance

```vyrn
fn editDistance(a: String, b: String) -> Int64
```

The optimal-string-alignment edit distance between `a` and `b`, in bytes:
insertions, deletions, substitutions and adjacent transpositions, each
costing 1.
