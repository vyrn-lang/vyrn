# RFC-0018 — The JSON Codec: `toJson` / `fromJson`

- **Status:** Implemented
- **Depends on:** RFC-0009 (`Validation<T>`/`Issue`), RFC-0003 (validated
  types), the jsonSchema emitter/importer (the schema side of this coin)

> **Implementation notes.** Shipped across all three backends, byte-identical
> — including every encoded string AND every `Issue`'s key/path/message.
>
> - The backend-neutral heart lives in `vela_frontend::codec`: the codability
>   predicates, the exact-integer parser, and the locked Issue wording, so the
>   interpreter and the code generator read message bytes from one source.
> - **Number representation (the one real design choice):** the schema parser
>   in `schema.rs` stores every number as `f64`, which cannot decode an
>   `Int64` exactly. The codec therefore uses a *sibling* parser whose number
>   token keeps its verbatim source text plus an `is_int` flag; an integer
>   target is decoded by parsing that text directly (`i64`/`u64` with a range
>   check), never through a double. `9007199254740993` round-trips.
> - Interp: a direct Rust walk of the value. Native/wasm: a JSON DOM + parser
>   + canonical encoder live in the C runtime shim (the parity-critical string
>   work — number formatting through the same `snprintf` that backs
>   `toString`, minimal escaping, parser error wording); the per-record-type
>   encode/decode logic is generated as LLVM IR (the `emit_validation`
>   precedent), with enums/options/arrays/validated scalars inlined and nested
>   records resolved to a call (recursion-safe).
> - Decode's `where` check derives from the **same** predicate lowering as the
>   trap path (`emit_predicate_cond`), and only runs on a cleanly-decoded value
>   — so the two paths never drift.
> - `examples/jsoncodec.vela` is the three-way parity showcase.

> **Motivation.** Vela can describe its types on the wire (jsonSchema, both
> directions) but cannot move *values* across it: there is no way to encode a
> record to JSON or decode JSON into one. This codec is the foundation of the
> RPC layer (RFC-0019) — and the place where validated types earn their keep:
> **decoding runs every `where` clause** and reports failures as structured,
> accumulated `Issue`s, not traps.

---

## Surface

```vela
type User = { name: Username, age: Age, nick: Option<String> }

let s = toJson(u)                          // String (canonical, deterministic)
let v = fromJson(User, s)                  // Validation<User>
match v {
    Valid(u)        => ...,
    Invalid(issues) => ...,                // Array<Issue>, every problem at once
}
```

- **`toJson(x) -> String`** — any *codable* value (below). Deterministic
  canonical output: record fields in declaration order, no whitespace,
  numbers rendered exactly as Vela's canonical `toString`, minimal JSON
  string escaping (`\" \\ \n \t \r`, `\u00XX` for other control bytes).
  `None` record fields are **omitted**; a bare `Option` encodes as `null`.
- **`fromJson(TypeName, s) -> Validation<T>`** — type-directed (the
  `schemaOf(TypeName)` precedent). Never traps; every problem is an `Issue`.

## The codable domain (v1)

Scalars (`Int64`, sized ints, `Float64`, `Float32`, `Bool`, `String`),
validated scalars, records (nested, incl. inline-refined fields),
`Option<T>`, `Array<T>`, payload-less enums (↔ JSON strings, matching the
jsonSchema `enum` emission). **Not codable** (checker error naming the
offender): payload enums (incl. `Result`), `Ref`, `Task`, `Template`,
`ArrayN` as a decode target (encode is fine — it is just an array).
Payload-enum encoding is the named v2 (a tagging decision that must land
together with its jsonSchema story).

## Decode semantics (locked)

- Unknown JSON fields are **ignored** (forward compatibility; matches JSON
  Schema's `additionalProperties` default).
- `Option<T>` accepts absent **or** `null` → `None`.
- Integers parse **exactly** (never through f64) — a non-integral or
  out-of-range number for an `Int64`/sized-int target is an `Issue`, as is
  a sized-int width violation.
- **Every `where` clause runs**, through the same validation machinery as
  every other boundary; failures accumulate (per RFC-0009) instead of
  stopping at the first.

### Issue vocabulary (locked)

`Issue { key, path, message }` with `path` in dotted/indexed form
(`""` for the root, `"age"`, `"items[2].name"`):

| key | when | message style |
|---|---|---|
| `json.parse` | malformed JSON (path `""`) | the parser's error |
| `json.type` | wrong JSON type / bad number for target | `expected <what>, found <what>` |
| `json.missing` | required field absent | ``missing required field `name` `` |
| `validate` | a `where` clause is false | the canonical validation wording for that type |

Exact message bytes are part of the parity surface: **identical across
interp, native, and wasm** — the invariant of this RFC.

## Laws (pinned by tests)

- Round-trip: `fromJson(T, toJson(x)) == Valid(x)` for every codable `x`.
- Determinism/idempotency of encode; schema coherence: everything `toJson`
  emits validates against `jsonSchema(T)`.

## Implementation shape (mechanism freedom, invariants fixed)

Interp: direct Rust walk. Native/wasm (one IR): suggested route — a generic
JSON parse runtime (C-shim/IR, the regex-runtime precedent) producing a
traversable form, walked by **per-type generated decode/encode functions**
synthesized at compile time (the `jsonSchema`-string / `emit_validation`
precedent); reuse the existing coercion/validation emission so `where`
checks are literally the same code paths. Whatever the mechanism: byte-
identical outputs and Issues, and `toJson`/`fromJson` are pure (spawn-safe
NOT granted though — keep them out of consteval; runtime only).

## Out of scope

Payload enums / `Result` on the wire (v2, with tagging + schema),
streaming/incremental parsing, non-JSON codecs (the seam is RFC-0019's),
custom serialization hooks, pretty-printing.
