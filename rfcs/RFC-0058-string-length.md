# RFC-0058 — `String.length` Is a Lie: `byteLength` / `charCount()`

- **Status:** Locked design
- **Depends on:** the String = UTF-8 bytes decision (documented 2026-07-16),
  RFC-0022 (`s[i]: UInt8`), the remove-with-hint precedent (unsized
  `Int`/`Float` removal)
- **Evidence (user):** "why length on string mean count of bytes? It is
  cringe and error prone." — correct. Byte length is the defensible
  systems primitive (Rust `len()`, Go `len()`), but the NAME `.length` on
  a TS-lookalike language promises characters to every TS refugee and
  delivers bytes: `"héllo".length == 6`.

The fix is not changing what it counts (a hidden O(n) scalar count would
be its own lie) — it is **refusing to answer an ambiguous question**.

---

## Surface

- **`String` loses `.length`.** Using it is a checker error with the
  established hint style:
  `String has no `length`: use `byteLength` for bytes or `charCount()` for Unicode scalars`
  (a real, targeted diagnostic — not the generic unknown-member error).
- **`s.byteLength: Int64`** — O(1), the byte count. Every existing call
  site means this; the migration is a rename with byte-identical output.
- **`s.charCount() -> Int64`** — O(n), the number of Unicode scalar
  values. Method-call shape on purpose: the parentheses hint the cost.
  Strings are validated UTF-8 at construction, so this is exactly the
  count of non-continuation bytes (`b & 0xC0 != 0x80`); implement it as a
  builtin with that loop in all three backends (bitwise ops exist,
  RFC-0045) — byte-identical everywhere.
- **`s[i]` stays `UInt8`** and all byte-offset APIs (`substring`,
  `indexOf`, `slice`, …) stay byte-indexed — that part was already honest.
- `Array<T>.length` and `SmallArray.length` are untouched.
- Grapheme clusters: out of scope, permanently until someone ships Unicode
  segmentation tables; the RFC says so instead of pretending `charCount`
  is "characters". Doc comments on both members must state exactly what
  they count.

## Migration (this RFC, not deferred)

- Mechanical `s.length` → `s.byteLength` across `std/`, `examples/`,
  compiler-embedded snippets, and tests — semantics preserved, so every
  example's output is byte-identical and parity proves it.
- `vyx`/`ui`/generator-EMITTED code that calls `.length` on strings must
  emit `byteLength` too (emit-gen diffs reviewed; behavior identical).
- LSP: member completion on a `String` offers `byteLength`/`charCount`;
  hover documents the units. The editor snippets/grammar don't care.

## Verification

1. Checker: `s.length` errors with the pinned hint wording; arrays
   unaffected.
2. `charCount`: ASCII == byteLength; `"héllo"` → 6 bytes / 5 scalars; a
   4-byte emoji → 4 / 1; empty string → 0 / 0. Three-way parity on a
   dedicated example printing both counts.
3. Whole-repo migration compiles with zero `.length`-on-String left
   (grep-gated in the test suite is fine); all example outputs
   byte-identical (parity), emit-gen diffs reviewed.
4. Full suite + LSP + parity green; 0 new clippy warnings; LSP rebuild +
   hash-verified redeploy.

## Out of scope

Grapheme clusters, `chars()` iteration (an iterator story is bigger than
this rename; `charCount` answers the counting question today), changing
any byte-offset API, and a `Char` type (RFC-0057 deliberately declined).
