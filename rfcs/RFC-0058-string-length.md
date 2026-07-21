# RFC-0058 — `String.length` Is a Lie: `byteLength` / `charCount()`

- **Status:** Implemented
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

---

## As landed

`String` lost `.length`; `byteLength` and `charCount()` are its two honest
answers.

**`byteLength` is the renamed lowering.** The old `Type::Str if field ==
"length"` path became `byteLength` verbatim in every backend: checker
(`Expr::Field` → `Int64`), interp (`Val::Str` → `s.len()`), `vyrn-codegen`
(`@__vyrn_strlen`), consteval (const byte length in refinement predicates), the
ownership analysis (a `.byteLength` read is non-escaping, like the old
`.length`), and the JSON-Schema bridge (`is_length_of_value` reads
`value.byteLength`; the emitter generates `value.byteLength` from
`minLength`/`maxLength`, byte-exact round-trip preserved). `Array`/`SmallArray`/
`Map` `.length` are untouched.

**`.length` on a String** is a targeted checker error with the pinned hint
`String has no `length`: use `byteLength` for bytes or `charCount()` for Unicode
scalars` — not the generic unknown-member diagnostic.

**`charCount()` is a builtin method.** Method syntax `s.charCount()` lowers to
an unspellable internal `@charCount` (exactly like `toString`→`@str`), so a free
`charCount(s)` never reaches it — which is why `examples/encoding.vyrn` can keep
its own user `fn charCount`. It counts non-continuation bytes
(`b & 0xC0 != 0x80`) of the validated UTF-8 string in all three backends: interp
(a filter/count), and native+wasm via a new runtime-shim helper
`__vyrn_charcount` added to `RUNTIME_SHIM` (compiled into both the native and the
wasm build — byte-identical). Verified: ASCII `charCount == byteLength`; `"héllo"`
→ 6 / 5; a 4-byte emoji → 4 / 1; `""` → 0 / 0.

**Deviation:** none of substance. `charCount` being method-only (via `@charCount`)
rather than a bare builtin function is the established precedent for method-only
builtins and is what lets the user-defined `charCount` in `encoding.vyrn` coexist.

**Migration (done, not deferred).** `s.length` → `s.byteLength` across the whole
repo, distinguishing String from `Array`/`Map` receivers by driving the checker's
new targeted error to a fixpoint (String `.length` errors → renamed; a mistaken
`Array.byteLength` would have re-errored — none did). **~122** call sites in the
corpus (115 in `.vyrn`, 7 in `.vyx` component bodies/templates) plus ~40 in Rust
test/source fixtures. Generator-EMITTED code was migrated at the source: `std/ui`'s
`uiFirst` head-runtime emits `a.byteLength`; `value.length` refinements in
`std/graphql` and every `wire`/`contract`/`api` example emit `value.byteLength`.
All example outputs stayed byte-identical (three-way parity proves it; emit-gen
diffs are the mechanical `.length`→`.byteLength` rename only, behaviour
unchanged). A dedicated example `examples/bytecount.vyrn` prints both counts.

**LSP.** Member completion on a `String` offers `byteLength` (a field, unit
documented: "UTF-8 byte count (O(1))") and `charCount()` (a method, "number of
Unicode scalar values (O(n))"); `.length` is absent.

**Verification.** Checker test for the pinned hint (arrays unaffected);
`charCount` parity across scripts; the whole corpus compiles with zero
String-`.length` remaining (the checker is the gate — a repo-wide grep can't tell
String from `Array`, so the type checker + three-way parity gate it instead).
Full workspace suite **989** passing; `vyrn-lsp` **40** passing;
interp==native==wasm parity green; 0 new clippy warnings.

**LSP redeploy.** `editor/vscode/server/vyrn-lsp.exe` rebuilt (release) and
hash-verified equal to the build output:
`349340C826D71BE9631F5623E7D15CF2102C6A3DD608BEEB8A8DF8AD3E562633`.
