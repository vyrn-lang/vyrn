# RFC-0059 — `std/json` + the std Cleanliness Sweep

- **Status:** Locked design
- **Depends on:** RFC-0057 (byte literals — the sweep adopts them),
  RFC-0058 (`byteLength` — already migrated by then), RFC-0046
  (`std/strings`), RFC-0054 (`std/scan`, code quotes), RFC-0026 (enum
  Array payloads — the `Json` tree rides the soundness fix)
- **Evidence (user):** "Doesn't it look messy?" — the survey: `std/` holds
  THREE private JSON hand-rollers (tw reader, i18n reader, openapi
  writer-by-concat), five-way duplicated string/byte helpers that
  `std/strings` already exports, parallel-array pseudo-structs, a dead
  `twStrLit` cited by five comments as the module's choke point, an O(N²)
  `twPush`, and fossils like `uiAppendStr`.

Two parts: **§1 one shared JSON module**, **§2 the sweep** that deletes
the duplication the survey found. Same dispatch so the churn lands once.

---

## 1. `std/json`

A comptime-friendly module (plain Vyrn, importable anywhere; generators
are the first consumers).

### The value tree

```vyrn
export type JsonField = { key: String, value: Json }
export type Json = JNull
                 | JBool(Bool)
                 | JNum(String)      // the RAW number text — no float round-trip
                 | JStr(String)
                 | JArr(Array<Json>)
                 | JObj(Array<JsonField>)
```

`JNum` carries the validated raw text on purpose: generators compare and
re-emit numbers; nobody needs float semantics at comptime, and raw text
makes emit → parse → emit byte-stable.

### Reader — strict where the hand-rollers were lenient

`export fn parse(src: String) -> Result<Json, String>` — errors carry
`line N, col M: <reason>`.

- Commas REQUIRED between members/elements; trailing commas REJECTED
  (both silently accepted by tw's reader today — that leniency is a bug,
  and fixing it is a deliberate behavior change; all in-repo themes and
  locales are valid JSON, so nothing in-repo breaks).
- Duplicate object keys REJECTED, naming the key (tw silently emitted
  duplicate classes).
- Full escape set including `\uXXXX` with surrogate pairs → UTF-8 (tw
  rejected `\u` outright; a general reader cannot). Lone surrogates are
  errors.
- Numbers validated to the JSON grammar, stored raw.
- Object field order preserved (source order — deterministic generators
  depend on it).

### Writer — kills emit-by-concat

- `export fn emit(j: Json) -> String` — compact, correct escaping
  (including control chars as `\u00XX`), field order as stored.
- `export fn emitPretty(j: Json, indent: Int64) -> String` — the
  `openapi` face.
- Round-trip law: `parse(emit(j)) == Ok(j)` for every tree `parse` can
  produce (test-pinned).

### Migrations (in this RFC)

- `std/tw`: delete `twScanString`/`twParseTheme` (~120 lines) — parse via
  `std/json`, then a small flatten adapter (dotted keys, the array→
  `safelist.N` projection). Generated module BYTE-IDENTICAL for valid
  themes (emit-gen goldens); invalid-but-previously-tolerated themes now
  fail with the strict reader's named errors (documented behavior change).
- `std/i18n`: same — its reader dies, the flatten adapter feeds the
  existing pipeline. Byte-identical emit-gen for the existing locales.
- `std/openapi`: build a `Json` tree, `emitPretty` it. If the canonical
  writer's bytes differ from the hand-concat document (escaping,
  whitespace), the diff is reviewed and the golden updated DELIBERATELY —
  the document must stay a valid, semantically-identical OpenAPI spec
  (pin with a parse-back test).

## 2. The sweep

- **Byte literals** (RFC-0057) replace bare byte numbers across `std/`
  scanners (`vyx` 59, `i18n` 46, `tw` 31, `graphql` 13, `ui` 12, `html`,
  `rpc`, `connect`). No behavior change — same integers after lexing.
- **`std/strings` adoption**: delete the private copies (`twJoin` →
  `joinWith`, `twSliceStr` → `substring`, `twSkipWs`/`twIsWs` → `std/scan`
  or strings equivalents, and the same in i18n/ui/vyx where a std export
  exists). An Array-contains helper that several modules copy moves to
  `std/arrays` once if it isn't there already.
- **Records over parallel arrays**: `TwVocab` → `Array<{name, decl}>`,
  `TwAxis` → `Array<{token, value}>`, i18n's `LocaleData`/`Branches`
  likewise — index-coupled pairs die.
- **tw specifics from the review**: delete dead `twStrLit` + fix the five
  comments that cite it (the choke point is `twEmitCss`); guard
  `twBlockLit` against the empty range (no more latent `j / 0`); replace
  O(N²) `twPush` with direct `mut` array building.
- **ui**: delete `uiAppendStr` and sibling fossils that predate the
  language fixes that made them pointless.
- **Err-swallows**: where a `stringFromBytes` failure is genuinely
  impossible (ASCII-delimited slices), route through ONE shared helper
  with a doc comment saying why it is infallible, instead of six copies
  of `Err(err) => ""`.

**The bar for the whole sweep: `vyrn emit-gen` byte-identical for every
example, all example outputs byte-identical (three-way parity), zero
behavior change outside the documented strict-JSON errors.**

## Verification

1. `std/json` unit tests: escapes incl. surrogate pairs, every strictness
   rejection with pinned wording + line/col, round-trip law, field-order
   preservation.
2. tw/i18n emit-gen goldens byte-compared before/after; openapi document
   parse-back-equal (and byte-diff reviewed if any).
3. Sweep: emit-gen byte-identical repo-wide; grep-gates for the deleted
   patterns (no `Err(err) => ""` outside the shared helper, no bare-byte
   comparisons left in the migrated scanners where a literal exists).
4. Full suite + LSP + three-way parity green; `vyrn fmt --check` clean;
   0 new clippy warnings; LSP redeploy only if compiler changed (state
   which, hash-verify if so).

## Out of scope

A runtime JSON-to-user-types decoder (the wire codec RFC-0028/0031 story
already covers typed boundaries), streaming/SAX parsing, JSON5/comments,
and float semantics for `JNum`.
