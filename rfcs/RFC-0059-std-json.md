# RFC-0059 — `std/json` + the std Cleanliness Sweep

- **Status:** Implemented
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

---

## As landed

Both parts shipped. `std/json` is a new 752-line module; the three JSON
hand-rollers are gone; the sweep landed across nine std modules. `vyrn emit-gen`
is **byte-identical repo-wide** except the one deliberate, documented `openapi`
document change. Workspace **990** passing (was 989 + the new `std/json` runner),
`vyrn-lsp` **40** passing, three-way parity **5** suites green, `vyrn fmt
--check` clean, **0 new** clippy warnings.

### §1 `std/json` — what shipped

The `Json` tree (`JNull | JBool(Bool) | JNum(String raw) | JStr(String) |
JArr(Array<Json>) | JObj(Array<JsonField>)`, `JsonField = {key, value}`), a
strict recursive-descent reader, a canonical writer (`emit` compact +
`emitPretty`), and structural equality. 15 inline tests + a Rust runner
(`compiler/vyrn-cli/tests/json.rs`) pin: the full escape set including `\uXXXX`
surrogate pairs to UTF-8, every strictness rejection with pinned `line N, col M`
wording (trailing comma, missing comma, duplicate key named, lone high/low
surrogate, bad number), field-order preservation, and the round-trip law.

**Locked-point deviations (both forced by the language, implemented as the
closest sound thing and documented in the module header):**

1. **The reader is `parseJson`, not `parse`.** `parse` is a reserved language
   builtin (`parse(String) -> Option<Int64>`), so a user module cannot define
   it. `emit`/`emitPretty` land as locked.
2. **`jsonEq` is canonical-emit equality**, not a hand-written recursive
   comparator. The locked law `parse(emit(j)) == Ok(j)` cannot be written
   literally: `==` is scalar-only (the checker restricts it to matching
   numeric/Bool/String), and there is no wildcard `_` pattern, so a structural
   comparator would need every variant pair spelled out. `emit` is injective
   over `Json` (distinct kinds carry distinct delimiters, object field order and
   raw number text are preserved), so `emit(a) == emit(b)` **is** exact
   structural equality; the round-trip test pins it via `jsonEq` plus the
   independent `emit(parse(emit(j))) == emit(j)` idempotence assertion.

Two further language facts shaped every migration: `match` arms and
`if`-expression branches must each be a **single expression** (no
multi-statement blocks, no `{ return ... }` arms — Result early-return uses the
`?` operator; loop-bearing arms are extracted to helper fns), and enum `match`
must be **exhaustive** (no `_`). This is why the readers/writers are factored
into many small single-expression helpers.

### §1 migrations — what moved

- **`std/tw`** (1255 -> 1048): the ~120-line private reader
  (`twScanString`/`twParseTheme`/`twUnescByte`/`TwScanStr`) deleted -> `parseJson`
  + a recursive flatten adapter (`twFlatten...`, dotted keys, `safelist.N` array
  projection). Byte-identical emit-gen for every theme.
- **`std/i18n`** (1574 -> 1425): the ~150-line private reader
  (`ScanStr`/`unescByte`/`scanString`/`parseLocaleJson` hand-parse) deleted ->
  the same `parseJson` + flatten-adapter shape (strings + nested objects only).
  Byte-identical for every locale.
- **`std/openapi`** (300 -> 298): the generated `openapiJson()` no longer
  hand-concatenates the document (the old `acc = acc + "..."` writer + private
  `oaEscBody` JSON escaper). It builds a `std/json` `Json` tree — envelope/paths/
  422 baked as compact JSON constants and `parseJson`d in, schema bodies still
  runtime `jsonSchema()` calls — and returns `emitPretty(doc, 2)`.

Both readers are now **strict**: duplicate keys and trailing commas the old
lenient readers silently accepted are rejected with named errors (the intended
behavior change; all in-repo themes/locales are valid JSON, so nothing in-repo
regressed).

### openapi byte-diff verdict

The document is now **pretty-printed (2-space indent)** instead of compact, and
the generated module source changed (concat -> tree). It remains a **valid,
semantically identical OpenAPI 3.1 spec**: the `exports.rs` parse-back golden
(`openapi 3.1.0`, `info`, paths in declaration order, `$ref`s, the 422 Issues
shape, sorted `components/schemas`, `$id` scoping, a validated `minimum`, a
`Result -> oneOf`, a `Map -> additionalProperties`, byte-stable across two runs)
passes **unchanged**. This is the RFC's one allowed exception: only
`examples/bin/server.vyrn` and `examples/shelf/server.vyrn` emit-gen differ, and
both diffs are **openapi-only** (verified: zero i18n/tw/vyx/ui/graphql generated
markers).

### §2 sweep

- **Byte literals** replace bare character-byte comparisons across the scanners
  (tw 31, i18n ~46, vyx ~60, graphql ~25, ui ~14, html 6, connect 2, rpc 1).
  HTTP status codes / lengths / counts stayed numeric.
- **`std/strings`/`std/arrays` adoption**: private `join`/`slice`/`contains`/
  `trim`/whitespace copies (`twJoin`/`twContains`/`twSliceStr`, `vyxJoin`/
  `vyxContains`/`vyxTrim`, `uiJoinList`/`uiContainsStr`/`uiTrimStr`, i18n's
  `listContains`/`trimStr`/`indexOf`, openapi's `oaListContains`, html's
  `bytesToStr`) deleted in favor of `joinWith`/`substring`/`includes`/`trim`.
  The Array-contains helper is **`includes`** in `std/arrays` (concrete
  `Array<String>` — `contains` is reserved, and a concrete parameter also
  accepts fixed-size array literals a generic `Array<T>` does not). Byte-array
  slicers (`vyxSlice`/`uiSliceStr`) were kept — `substring` is String-only.
- **Records over parallel arrays**: `TwVocab`->`Array<TwClassDef>`,
  `TwAxis`->`Array<TwPair>`, `TwTheme` keys/vals->`entries: Array<TwEntry>`;
  i18n `LocaleData` keys/vals->`entries: Array<LocaleEntry>` and
  `Branches` selectors/texts->`branches: Array<Branch>`.
- **tw specifics**: dead `twStrLit` deleted + the five stale comments that cited
  it fixed (the choke point is `twEmitCss`); `twBlockLit` now guards the empty
  range (`hi <= lo -> ""`, killing the latent `j / 0`); the O(N^2)
  value-semantics `twPush` replaced with direct `modify`-array building.
- **Err-swallows**: the provably-infallible `stringFromBytes` sites (ASCII
  transforms, ASCII-boundary slices/rebuilds of already-valid UTF-8) route
  through one shared documented `fromBytesOr(bytes, fallback)` in `std/strings`.
  Genuinely-fallible `readFile`/`listDir` swallows and emitted-code (`vyrn"""`)
  swallows were correctly left as-is.

### Deferred / not done (with reason)

- **`uiAppendStr` fossil left in place.** It is a COPY-append (returns a new
  array; the source stays unchanged) used in `uiScanAll`'s recursion so sibling
  branches keep the original prefix. A bare `arr.push(s)` would wrongly mutate
  the shared prefix, and Vyrn has no byte-identical single-expression
  copy-append idiom (`arr + [s]` is not array concat — it errors). Preserving
  byte-identity won over deleting it; documented here rather than risk a silent
  regression.
- **Pre-existing native-backend bug unrelated to this RFC**: the subdir server
  examples (`examples/bin/server`, `examples/shelf/server`) fail the NATIVE
  backend build (`alloca void` / struct `insertvalue` mismatch); this reproduces
  identically on the base commit `e8cb43d` (verified) and is already
  chip-filed — not a regression from this work. Three-way parity (which excludes
  those subdir servers) is green.

### LSP

**No LSP redeploy.** No compiler CRATE source changed — the only change under
`compiler/` is the new integration test `compiler/vyrn-cli/tests/json.rs`, which
does not enter the release/LSP binary. `std/` ships with the repo (loaded from
disk), so the migrations need no rebuild. The deployed
`editor/vscode/server/vyrn-lsp.exe` hash is unchanged and verified:
`349340C826D71BE9631F5623E7D15CF2102C6A3DD608BEEB8A8DF8AD3E562633`.

### Line-count deltas

| module | before | after | delta |
| --- | ---: | ---: | ---: |
| `json` (new) | 0 | 752 | +752 |
| `tw` | 1255 | 1048 | -207 |
| `i18n` | 1574 | 1425 | -149 |
| `ui` | 1676 | 1615 | -61 |
| `vyx` | 3520 | 3460 | -60 |
| `connect` | 366 | 360 | -6 |
| `graphql` | 693 | 689 | -4 |
| `html` | 723 | 719 | -4 |
| `openapi` | 300 | 298 | -2 |
| `rpc` | 422 | 423 | +1 |
| `strings` | 323 | 330 | +7 |
| `arrays` | 79 | 94 | +15 |

Net across `std/`: **+1633 / -1351**. Removing the three hand-rollers and the
five-way-duplicated helpers pays for the shared reader/writer and then some
everywhere except the new module itself.
