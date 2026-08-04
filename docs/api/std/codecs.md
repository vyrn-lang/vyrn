# std/codecs

std/codecs — hex, base64 and percent encoding, written in Vyrn (RFC-0078 M4b).

The six codec builtins — `hexEncode`, `hexDecode`, `base64Encode`,
`base64Decode`, `urlEncode`, `urlDecode` — are the clearest case RFC-0078
makes. They are pure byte transforms: no syscall, no allocator trick, no type
knowledge, and (unlike every other builtin) **no C shim implementation at
all** — the native path is hand-written LLVM IR printed by the textual
emitter, and the interpreter holds the same algorithm again in Rust. Two
definitions of a table lookup and a shift, plus a third owed to the direct
wasm backend.

This module is what one definition looks like: `bytes(s)` gives the input as
`Array<UInt8>`, `stringFromBytes(..)` turns the output back into a `String`,
and everything between is a `while` loop and the bitwise operators from
RFC-0045. No primitive was missing — which is the finding, since M4a needed
two (`floatBits`/`floatFromBits`) and M3 is still waiting on a ruling.

**These six ARE the builtins now** (RFC-0078 M4c). M4b(1) proved the Vyrn
versions equal to them over 6,354 comparisons with the builtin as the oracle;
M4c routed `hexEncode`, `hexDecode`, `base64Encode`, `base64Decode`, `urlEncode`
and `urlDecode` into this module on every engine and deleted the duplicates —
521 lines of hand-written LLVM IR and 162 lines of Rust. The `V` suffix is
therefore a second spelling of the same function rather than a rival to it;
`tests/codecs.rs` carries the old oracle forward as a pinned digest, since a
comparison would now be `x == x`.

**One behavioural change, and it is a bug fix.** A decoder whose bytes contain
`0x00` — `hexDecode("00")`, `base64Decode("AA==")`, `urlDecode("%00")` — returns
`None`, because RFC-0014's rule is that a Vyrn `String` cannot hold a NUL and
`stringFromBytes` enforces it. The deleted builtin answered `Some`, and did not
agree with itself: the interpreter kept a Rust `String` with an embedded NUL
while the native path returned a NUL-terminated `char*` silently truncated at
that byte. No example decodes a NUL, so parity had never looked. Measured across
the swap, that is **16 of 6,354 rows** and every one of the 16 is a NUL row —
`tests/codecs.rs` records both digests and pins the rows individually.

## hexEncodeV

```vyrn
fn hexEncodeV(s: String) -> String
```

A string's UTF-8 bytes as lowercase hex, two digits per byte.

## hexDecodeV

```vyrn
fn hexDecodeV(s: String) -> Option<String>
```

Hex text back to a `String` — `None` on an odd length, a non-hex digit, or
bytes that are not valid UTF-8. Case-insensitive, as the encoder's inverse
has to be to read anyone else's output.

## base64EncodeV

```vyrn
fn base64EncodeV(s: String) -> String
```

A string's UTF-8 bytes as base64: three bytes to four digits, with `=`
padding for a final group of one or two.

## base64EncodeBytes

```vyrn
fn base64EncodeBytes(b: Array<UInt8>) -> String
```

The same, over bytes that are not text. A digest is the case this exists for
(RFC-0074 M3b's handshake base64s twenty SHA-1 bytes): it can hold a NUL and
need not be UTF-8, so it cannot make the trip through a `String` first.

## base64DecodeV

```vyrn
fn base64DecodeV(s: String) -> Option<String>
```

Base64 text back to a `String` — `None` unless the length is a multiple of
four, every digit is in the alphabet, the padding is confined to the final
group and is `=` or `==` (never `=X`), and the bytes are valid UTF-8.

## urlEncodeV

```vyrn
fn urlEncodeV(s: String) -> String
```

A string's UTF-8 bytes percent-encoded, uppercase hex.

## urlDecodeV

```vyrn
fn urlDecodeV(s: String) -> Option<String>
```

Percent-encoded text back to a `String` — `None` on a truncated or non-hex
escape, or bytes that are not valid UTF-8. Any byte that is not `%` passes
through, so a raw space or `+` decodes to itself.
