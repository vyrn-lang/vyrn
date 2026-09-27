# std/text

std/text: UTF-8 decoding and byte-offset line and column, in Vyrn.

The engines route `s.charCount()` to `charCountV`, `lineAt` to `lineAtV` and
`colAt` to `colAtV`; `chars` is an ordinary export. `utf8Width` is the one
statement of what UTF-8 admits, and `decodeUtf8` and `stringFault` both ask it,
so `tests/text.rs` checks them against Rust's `std::str::from_utf8`, not against
each other.

`decodeUtf8` accepts `0x00` and `stringFromBytes` refuses it: a `String` is
NUL-terminated, so the two differ in what a `String` can hold, not in
what UTF-8 means.

## decodeUtf8

```vyrn
fn decodeUtf8(b: Array<UInt8>) -> Option<Array<Int64>>
```

The Unicode scalar values of `b`, or `None` if `b` is not valid UTF-8 as Rust's
`String::from_utf8` defines it.

## utf8Width

```vyrn
fn utf8Width(b: Array<UInt8>, i: Int64) -> Int64
```

The width of the UTF-8 sequence that starts at `i` in `b`, or 0 when the
bytes there start none.

`lo`/`hi` bound the first continuation byte, and they carry every overlong and
out-of-range rule: 0xE0 needs 0xA0+, 0xED stops at 0x9F (surrogates), 0xF0
needs 0x90+ and 0xF4 stops at 0x8F (past U+10FFFF).

## stringFault

```vyrn
fn stringFault(b: Array<UInt8>) -> Int64
```

What is wrong with `b` if it were made into a `String`: 0 nothing, 1 a NUL
byte, 2 not UTF-8.

Every engine calls this as the check half of `stringFromBytes` and keeps only
the build. The NUL scan covers the whole array first, so
`[0xFF, 0x00]` is a NUL error. ASCII steps inline so the digits `intStr` hands
in cost no call.

## chars

```vyrn
fn chars(s: String) -> Array<Int64>
```

The codepoints of `s`.

The `None` arm is unreachable: every boundary that builds a `String` validates
its UTF-8.

## charCountV

```vyrn
fn charCountV(s: String) -> Int64
```

The number of Unicode scalar values in `s`: a byte scan that counts every byte
that is not a continuation (`0b10xxxxxx`), with no allocation.

## lineAtV

```vyrn
fn lineAtV(b: Array<UInt8>, off: Int64) -> Int64
```

The 1-based line of byte offset `off` in `b`: one more than the LF bytes before
`off`. An offset past the end reads as the end, a negative one as 0. O(off).

## colAtV

```vyrn
fn colAtV(b: Array<UInt8>, off: Int64) -> Int64
```

The 1-based column of byte offset `off` in `b`, in bytes, not codepoints: the
byte after a two-byte codepoint on an otherwise empty line is column 3.

## showCps

```vyrn
fn showCps(a: Array<Int64>) -> String
```

The codepoints as comma-separated decimals, so a test compares exact scalar
values.
