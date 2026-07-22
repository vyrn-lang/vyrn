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
