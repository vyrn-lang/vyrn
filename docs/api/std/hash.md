# std/hash

std/hash — non-cryptographic byte hashing (RFC-0045).

FNV-1a over a byte sequence: for each byte, `h = (h ^ byte) * prime`, seeded
from the FNV offset basis. This is the canonical hash the bin dogfood wanted
but could not write before bitwise operators existed (it hand-rolled a weaker
polynomial rolling hash — no `^`). It is fast, well-distributed for short
keys, and deterministic across every backend (the mixing runs in `UInt64`
with wrapping multiply and xor, so interp/native/wasm agree bit-for-bit).

**Width:** 64-bit (FNV-1a-64) — Vigna/Fowler-Noll-Vo constants
(offset basis `0xCBF29CE484222325`, prime `0x100000001B3`). A 64-bit digest
keeps collisions negligible for content-addressing without a second round.

NOT cryptographic and NOT collision-resistant against an adversary — use it
for hash tables, content-addressed ids, and checksums, not for security.

## fnv1a

```vyrn
fn fnv1a(data: Array<UInt8>) -> UInt64
```

The FNV-1a-64 hash of a byte array. `h = offset; for b: h = (h ^ b) * prime`.

## fnv1aStr

```vyrn
fn fnv1aStr(s: String) -> UInt64
```

The FNV-1a-64 hash of a String's UTF-8 bytes — a convenience over [`fnv1a`]
(Vyrn strings are UTF-8 byte sequences, so this hashes the exact bytes).

## Hashable

```vyrn
protocol Hashable { fn hash(self) -> UInt64 }
```

A key is anything that hashes (RFC-0117). The one contract: equal values
return equal hashes. Beyond that the value is the implementation's — a
`Map`'s index mixes and masks it itself, and the hash decides nothing
observable (insertion order does, RFC-0028).

A heapless user type — a record of sized integers and `Bool` (nested
records and fieldless enums included), or a fieldless enum — keys a `Map`
once it declares `impl Hashable` (M2). The declaration is the obligation
and the type's callable hash; the builtin `Map` hashes the key's
canonical field bytes itself, exactly as it always hashed a `String`'s
bytes and an `Int64`'s bits without consulting the impls below. Equality
between keys is field-wise — padding is never compared.

## sha1

```vyrn
fn sha1(data: Array<UInt8>) -> Array<UInt8>
```

**SHA-1 is here as a handshake nonce transform, not as a security
primitive.** RFC 6455 §4.2.2 mandates it for exactly one purpose: the
WebSocket server proves it understood the upgrade by echoing
`base64(SHA-1(key + GUID))`, where the GUID is a published constant and the
key is a nonce the client just sent in the clear. Nothing about that step is
secret and nothing about it is a signature.

**Do not use it for anything else.** SHA-1 is collision-broken in practice
(SHAttered, 2017; chosen-prefix collisions, 2020), so it must not sign, must
not hash a password, must not authenticate a message and must not
content-address anything an adversary can influence. Vyrn ships no
cryptographic hash — for content addressing use [`fnv1a`], and for anything
that has to resist an attacker use a reviewed implementation outside this
module.

The digest is twenty bytes, big-endian, pinned against RFC 3174 §7.3's own
test vectors in `examples/sha1.vyrn`. The mixing runs in `UInt64` masked to
32 bits rather than in `UInt32`, so every engine agrees without depending on
a narrower type's overflow rule.

## sha1Hex

```vyrn
fn sha1Hex(s: String) -> String
```

The SHA-1 digest of a String's UTF-8 bytes as lowercase hex — the spelling
RFC 3174 §7.3's test vectors are written in. Read [`sha1`]'s warning first.
