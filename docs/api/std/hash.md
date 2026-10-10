# std/hash

std/hash: non-cryptographic byte hashing, and SHA-1 for the
WebSocket handshake.

FNV-1a-64 mixes in `UInt64` with wrapping multiply and xor, so every engine
agrees bit for bit. It is not collision-resistant against an adversary: use
it for hash tables, content-addressed ids and checksums, not for security.

## fnv1a

```vyrn
fn fnv1a(data: Array<UInt8>) -> UInt64
```

The FNV-1a-64 hash of a byte array.

## fnv1aStr

```vyrn
fn fnv1aStr(s: String) -> UInt64
```

The FNV-1a-64 hash of a String's UTF-8 bytes.

## Hashable

```vyrn
protocol Hashable { fn hash(self) -> UInt64 }
```

A key is anything that hashes. Equal values return equal hashes;
the value decides nothing observable, because a `Map` iterates in insertion
order.

A heapless user type (a record of sized integers, `Bool`, nested records and
fieldless enums, or a fieldless enum) keys a `Map` once it declares
`impl Hashable`. The builtin `Map` hashes the key's canonical field bytes
itself, without calling the impl, and compares keys field-wise, never
padding.

## sha1

```vyrn
fn sha1(data: Array<UInt8>) -> Array<UInt8>
```

SHA-1 (RFC 3174), for the WebSocket handshake only: RFC 6455 section 4.2.2
has the server echo `base64(SHA-1(key + GUID))`, and nothing in that step is
secret. SHA-1 is collision-broken: never use it to sign, hash a password,
authenticate a message or content-address attacker-influenced data.

The digest is twenty bytes, big-endian, pinned against RFC 3174 section 7.3
in `examples/sha1.vyrn`. A word is a `UInt32`, so every addition wraps
modulo 2^32 as the RFC requires.

## sha1Hex

```vyrn
fn sha1Hex(s: String) -> String
```

The SHA-1 digest of a String's UTF-8 bytes as lowercase hex. Read `sha1`'s
warning first.
