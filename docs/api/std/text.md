# std/text

std/text — UTF-8 decoding and byte-offset line/column, written in Vyrn
(RFC-0078 M4b).

Three builtins live here as ordinary Vyrn: `chars` (a `String`'s codepoints),
`lineAt` and `colAt` (the 1-based line and column of a byte offset). None of
them is retired by this module — this is the equivalence half of the
milestone, and the swap is a separate decision — but each is now written once
instead of once per engine, and `tests/text.rs` proves the Vyrn version
answers what the builtin answers over the whole surface including the
malformed one.

**M4a's question, asked again.** The finding worth carrying from the number
tier was that the irreducible primitive is a missing VIEW rather than a
missing operation: nothing could read a `Float64`'s bits, so every text ->
float route had to be a builtin. The string half needs no new view at all.
`bytes(s)` already exposes a `String` as its UTF-8 bytes and `stringFromBytes`
is the validated inverse, so decoding is expressible today — which is why this
file needs no compiler change of any kind, unlike `std/num`, which needed two.

**The validator is not duplicated, it is inverted.** `stringFromBytes` walks
Björn Höhrmann's DFA (`utf8d_table`, shared between the textual and direct
wasm backends since RFC-0077 M2g) to decide validity and throws the
codepoints away. `decodeUtf8` below decides the same question by first-byte
dispatch and keeps them. The two must agree on every byte string, and the test
asserts exactly that rather than trusting the tables to match.

**One deliberate disagreement, and it is not about UTF-8.** `stringFromBytes`
rejects an interior NUL *before* it looks at UTF-8 (RFC-0014's rule: a Vyrn
`String` is NUL-terminated, so it could not carry one). `0x00` is perfectly
valid UTF-8, so `decodeUtf8` accepts it and returns codepoint 0. That is a
difference in what a `String` can hold, not in what UTF-8 means, and it is
pinned as its own row rather than hidden.

## decodeUtf8

```vyrn
fn decodeUtf8(b: Array<UInt8>) -> Option<Array<Int64>>
```

The Unicode scalar values of `b`, or `None` if `b` is not valid UTF-8.

Rejects exactly what `String::from_utf8` and the DFA reject, by the standard
first-byte table: overlong forms (`0xC0`/`0xC1`, and `0xE0`/`0xF0` whose
continuation is too low), the surrogate range (`0xED 0xA0..0xBF`), anything
above U+10FFFF (`0xF4 0x90..`, and `0xF5..0xFF`), a lone continuation byte,
and a sequence truncated by the end of the buffer.

NOT a validator plus a decoder: the ranges that make a form overlong are the
same ranges the codepoint arithmetic uses, so checking and building are one
pass. Bails at the first bad byte — a partial decode has no caller, and
`stringFromBytes`, the oracle, is all-or-nothing too.

## charsV

```vyrn
fn charsV(s: String) -> Array<Int64>
```

The codepoints of `s` — the `chars` builtin, in Vyrn.

The `None` arm is unreachable and that is a property of the language rather
than an assumption: a `String` is validated UTF-8 at every boundary that can
build one, and `stringFromBytes` is the only route from arbitrary bytes. So
`chars` never sees a malformed input, which is why the malformed table below
is pinned against `stringFromBytes` instead.

## lineAtV

```vyrn
fn lineAtV(b: Array<UInt8>, off: Int64) -> Int64
```

The 1-based line number of byte offset `off` in `b` — the `lineAt` builtin, in
Vyrn. One more than the number of LF bytes before `off`; an offset past the
end reads as the end, and a negative one as 0.

This is the shape the builtin exists to avoid: `lineAt` is a builtin BECAUSE
the obvious loop is O(off) and a scanner asks once per node, which cost
`std/vyx` 122 ms of a 291 ms page compile. The interpreter memoizes a
line-start table per buffer; the native shim counts directly, exactly like
this. So the Vyrn version is not slower than every engine — it is slower than
one of them, and identical to the other.

## colAtV

```vyrn
fn colAtV(b: Array<UInt8>, off: Int64) -> Int64
```

The 1-based column of byte offset `off` in `b` — the `colAt` builtin, in Vyrn.

**The column counts BYTES, not codepoints**, and that was measured off the
builtin rather than assumed: both the interpreter (`off - lineStart + 1`) and
the shim (a backward walk to the LF) count bytes, so the byte after `é` on an
otherwise empty line is column 3 and not column 2. `std/vyx`'s wrapper
documents it as "chars since the last LF", which is wrong for any non-ASCII
line; RFC-0033 origin directives feed a C-style `#line`, where byte columns
are the convention anyway.

## showCps

```vyrn
fn showCps(a: Array<Int64>) -> String
```

The codepoints as text, for pinning: a decode is only interesting at the exact
scalar values, and `%s` of the rebuilt string would hide a wrong one that
still renders.
