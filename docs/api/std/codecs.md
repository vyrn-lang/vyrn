# std/codecs

std/codecs -- hex, base64 and percent encoding over UTF-8 bytes. These are the
`hexEncode`, `hexDecode`, `base64Encode`, `base64Decode`, `urlEncode` and
`urlDecode` builtins on every engine; `tests/codecs.rs` pins their output.

A decoder whose bytes contain `0x00` returns `None`: a Vyrn `String` cannot
hold a NUL, and `stringFromBytes` enforces it.

## hexEncode

```vyrn
fn hexEncode(s: String) -> String
```

A string's UTF-8 bytes as lowercase hex, two digits per byte.

## hexDecode

```vyrn
fn hexDecode(s: String) -> Option<String>
```

Hex text back to a `String`; `None` on an odd length, a non-hex digit, or
bytes that are not valid UTF-8. Reads either case.

## base64Encode

```vyrn
fn base64Encode(s: String) -> String
```

## base64EncodeBytes

```vyrn
fn base64EncodeBytes(b: Array<UInt8>) -> String
```

Base64 of bytes that are not text, such as a digest: they can hold a NUL and need not be UTF-8.

## base64Decode

```vyrn
fn base64Decode(s: String) -> Option<String>
```

Base64 text back to a `String`; `None` unless the length is a multiple of four,
every digit is in the alphabet, padding is `=` or `==` in the final group only,
and the bytes are valid UTF-8.

## urlEncode

```vyrn
fn urlEncode(s: String) -> String
```

A string's UTF-8 bytes percent-encoded, uppercase hex.

## urlDecode

```vyrn
fn urlDecode(s: String) -> Option<String>
```

Percent-encoded text back to a `String`; `None` on a truncated or non-hex
escape, or bytes that are not valid UTF-8. A raw space or `+` decodes to itself.
