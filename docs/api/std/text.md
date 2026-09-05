# std/text

std/text — UTF-8 decoding and byte-offset line/column, written in Vyrn
(RFC-0078 M4b).

Four builtins live here as ordinary Vyrn: `chars` (a `String`'s codepoints),
`charCount` (how many of them there are), `lineAt` and `colAt` (the 1-based line
and column of a byte offset). **`chars` and `charCount` are retired** — RFC-0078
M4c routed `chars` into `charsV`, deleting Rust's `str::chars` from the
interpreter and 82 lines of two-pass decoder from the textual emitter, and the
census that followed routed `charCount` into `charCountV`, deleting a Rust arm,
a C shim function, a line of emitted IR and ~30 lines of hand-written
`wasm-encoder`. **RFC-0094 M2 then took `chars` one step further**: it has a free
spelling, so it is an ordinary export named `chars` and a caller imports it.
`charCount` cannot follow — `s.charCount()` is method-only, so the name the
engines look up is `@charCount`, which no import can bring into scope.

`lineAt` and `colAt` moved neither time: the interpreter memoizes a line-start
table per buffer and a Vyrn library cannot (a generator may not touch module
state — comptime purity), and the loop below is O(off) where the memo is O(1).
That is worth 122 ms of a 291 ms `std/vyx` page compile, measured, so retiring
them is a decision about that cache rather than about capability. They stay
builtins and `lineAtV`/`colAtV` stay the thing they are proved against.

`tests/text.rs` is what proves it: the `chars` half is now a pinned digest over
~2,000 codepoints (a comparison against the builtin would compare `chars` with
itself), while the malformed table and the line/column table are still live
oracles against `stringFromBytes` and `lineAt`/`colAt`, neither of which moved.
`charCount` needed no new digest and that is the interesting part: `charCountV`
is a byte scan and `chars` is a full decode, so the two are independent
implementations of one fact, and `chars`'s side of that comparison is already
pinned to the pre-swap C and Rust.

**M4a's question, asked again.** The finding worth carrying from the number
tier was that the irreducible primitive is a missing VIEW rather than a
missing operation: nothing could read a `Float64`'s bits, so every text ->
float route had to be a builtin. The string half needs no new view at all.
`bytes(s)` already exposes a `String` as its UTF-8 bytes and `stringFromBytes`
is the validated inverse, so decoding is expressible today — which is why this
file needs no compiler change of any kind, unlike `std/num`, which needed two.

**The validator is not inverted any more, it is the same function.**
`stringFromBytes` used to walk Björn Höhrmann's DFA in each engine and throw
the codepoints away, while `decodeUtf8` decided the same question by
first-byte dispatch and kept them — two statements the test compared. RFC-0125
§3 M6's fifth slice made the check one Vyrn function every engine calls, so
`stringFault` and `decodeUtf8` now share `utf8Width` and cannot disagree.
`tests/text.rs` compares BOTH against Rust's `std::str::from_utf8` for that
reason: an oracle inside this file would compare the file with itself.

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

## utf8Width

```vyrn
fn utf8Width(b: Array<UInt8>, i: Int64) -> Int64
```

The width of the UTF-8 sequence that starts at `i` in `b`, or 0 when the
bytes there start none — RFC-0125 §3 M6, the third judgment's fifth slice.

**The one statement of what UTF-8 admits.** `decodeUtf8` above asks it and
then does the codepoint arithmetic; `stringFault` below asks it and steps.
Before this slice the same ranges were written four times: here, and once
per engine behind `stringFromBytes` — Rust's `String::from_utf8` in the
interpreter, Björn Höhrmann's DFA in the textual emitter's
`@__vyrn_utf8valid`, and the same DFA again in `std/runtime`'s `utf8Valid`
for the wasm route. The engines call this now and none of them decides.

`w` is the width (0 = this byte starts no sequence) and `lo`/`hi` bound the
FIRST continuation byte. Those bounds are the whole of the overlong and
out-of-range story: 0xE0 demands 0xA0+ (below that the value fits in two
bytes), 0xED stops at 0x9F (above it is a surrogate), 0xF0 demands 0x90+
and 0xF4 stops at 0x8F (past U+10FFFF). A width of 0 covers 0x80..0xC1 (a
lone continuation, and the two overlong two-byte leads) and 0xF5..0xFF.

## stringFault

```vyrn
fn stringFault(b: Array<UInt8>) -> Int64
```

What is wrong with `b` if it were made into a `String`: 0 nothing, 1 a NUL
byte, 2 not UTF-8 — RFC-0125 §3 M6, the third judgment's fifth slice.

**This is the CHECK half of `stringFromBytes`, and all three engines call
it.** The census's `string-nul` and `string-utf8` rows had three carriers
because the check and the BUILD were one function per engine, and the build
needs the raw-memory primitives `std/mem` fences. The check needs none: it
reads bytes and answers a number. So it is ordinary Vyrn, reached the way
`char-boundary` and `json-decode` are, and each engine keeps only its
build.

The NUL scan runs over the WHOLE array before the encoding is looked at,
because RFC-0014 orders the two: a `String` is NUL-terminated, so bytes
holding one are not representable rather than badly encoded, and
`[0xFF, 0x00]` is a NUL error and not an encoding error. ASCII is stepped
here rather than through `utf8Width`, so the digits `intStr` hands in cost
no call.

## chars

```vyrn
fn chars(s: String) -> Array<Int64>
```

The codepoints of `s` — the `chars` builtin, in Vyrn.

The `None` arm is unreachable and that is a property of the language rather
than an assumption: a `String` is validated UTF-8 at every boundary that can
build one, and `stringFromBytes` is the only route from arbitrary bytes. So
`chars` never sees a malformed input, which is why the malformed table below
is pinned against `stringFromBytes` instead.

## charCountV

```vyrn
fn charCountV(s: String) -> Int64
```

The number of Unicode scalar values in `s` — the `charCount` builtin, in Vyrn.

A UTF-8 continuation byte is exactly `0b10xxxxxx`, so every OTHER byte starts a
scalar and the count is a byte scan with no allocation. `examples/encoding.vyrn`
proved the same answer as `chars(s).length`, which is a shorter body and a worse
one: it builds an `Array<Int64>` the caller throws away.

RFC-0078's census found this the only builtin with no justification for being
one — no primitive (`byteLength` and `s[i]` are the whole substrate, the same
substrate `std/strpred` is built on), no trap, no `consteval` fold, and one
caller in the repository. It had four implementations: a Rust arm, a C shim
function, a line of emitted IR and ~30 lines of hand-written `wasm-encoder`.

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
