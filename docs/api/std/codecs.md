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

The names carry a `V` suffix on purpose: the builtins still exist and nothing
is swapped yet. `tests/codecs.rs` runs both over a wide corpus and asserts
they agree, so the equivalence is proved BEFORE any deletion — the same
ordering M2 and M3 used for their byte pins, and the reason those milestones
found bugs in a third engine rather than shipping them.

**One deliberate divergence, and it is the builtin that is wrong.** A decoder
whose bytes contain `0x00` — `hexDecodeV("00")`, `base64DecodeV("AA==")`,
`urlDecodeV("%00")` — returns `None` here, because RFC-0014's rule is that a
Vyrn `String` cannot hold a NUL and `stringFromBytes` enforces it. The
builtin answers `Some`, and it does not agree with itself: the interpreter
keeps a Rust `String` with an embedded NUL while the native path returns a
NUL-terminated `char*` that is silently truncated at that byte. No example
decodes a NUL, so parity has never looked. `tests/codecs.rs` pins the
divergence as a divergence rather than papering over it.

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
