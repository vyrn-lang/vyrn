# RFC-0024 — Payload Enums on the Wire (Codec v2)

- **Status:** Draft — approved for implementation
- **Depends on:** RFC-0018 (the codec — this is its named v2), the jsonSchema
  emitter/importer (the tagging decision must land in both, together)
- **Evidence:** RFC-0018 and RFC-0019 both defer exactly this; RPC contracts
  model errors as records+`Option` because `Result<T, E>` is not codable.

---

## The tagging decision (locked)

Externally tagged, arity-shaped — the encoding that keeps the existing
payload-less form as its degenerate case:

```vyrn
type Shape = | Circle(Int64) | Rect(Int64, Int64) | Unit
```

| variant | JSON |
|---|---|
| `Unit` (nullary) | `"Unit"` — a bare string, **identical to payload-less enums today** |
| `Circle(5)` (one payload) | `{"Circle":5}` — unwrapped |
| `Rect(2, 3)` (two+) | `{"Rect":[2,3]}` — an array |

- `Result<T, E>` becomes codable for free: `{"Ok":<T>}` / `{"Err":<E>}`.
  Generic payloads follow the instantiation (`Result<User, String>` — the
  codec already monomorphizes per concrete type).
- A **pure-nullary enum keeps today's compact encoding unchanged** (bare
  strings; its jsonSchema stays `{"enum":[...]}`) — zero migration.
- Codability of an enum = codability of every payload (checker names the
  offending variant/payload otherwise). `Validation<T>` itself stays
  non-codable v1 (its `Invalid` carries `Array<Issue>` — allowing it is a
  one-line follow-up once this lands, if wanted; not load-bearing now).

## Decode semantics (locked)

- A payload variant decodes from an object with **exactly one key**; that
  key must be a known variant. Wrong shape / unknown variant →
  `json.type` Issue: ``expected one of `Circle`, `Rect`, `Unit`, found …``
  (kind, not value — the RFC-0018 uniformity rule).
- Payload arity and types check with paths `shape.Circle` (single) /
  `shape.Rect[1]` (tuple index); `where` clauses on validated payloads run
  and accumulate, as everywhere.
- A nullary variant decodes from its bare string (and ONLY that — no
  `{"Unit":null}` alternate spellings; one wire form per value).

## The schema story (lands together, round-trip exact)

- **Emitter:** a payload enum emits `oneOf`: nullary variants as
  `{"const":"Unit"}`; payload variants as single-property objects
  (`{"type":"object","properties":{"Circle":<payload schema>},
  "required":["Circle"]}`, tuple payloads via `prefixItems`), replacing the
  current honest-lossy `$comment`.
- **Importer:** recognizes exactly that `oneOf` shape back into a Vyrn
  enum — emit → import → re-emit stays byte-exact (the pinned law). Any
  other `oneOf` remains a hard error (exactly-or-not-at-all).

## Ripples (part of this RFC)

- `std/rpc` generation-time contract checks defer to the codability
  predicate — `Result`-returning procedures become legal automatically.
  Document the semantics: an `Err` return is an application-level outcome —
  **200** with `{"Err":…}` on the wire (422 remains reserved for request
  *validation* failures). The fullstack demo gains a `Result`-returning
  procedure to prove it end-to-end.
- `fromJson`/`toJson` per-type generated IR grows an enum path (tag switch;
  the interp mirrors). Round-trip law extends over the new domain
  (`fromJson(T, toJson(x)) == Valid(x)` incl. nested enums-in-records,
  enums-in-arrays, `Option<Result<…>>`).

## Out of scope

Renaming variants on the wire (serde-style attributes), internally/
adjacently tagged alternates, `Validation<T>` codability (noted above),
protobuf numbering (still reserved for the Connect work).
